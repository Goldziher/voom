//! Walking a git work tree from its index instead of from its directories.
//!
//! Opening a directory is the walk's dominant cost, and on a machine whose endpoint-security
//! agents vet every `open` it is most of the run: measured over 43 linked worktrees of one
//! repository — 905,000 directories — `open` alone took 16.6 s of a 21 s listing pass, while an
//! `lstat` of every one of them took 3.6 s. Almost none of those directories can hold an
//! artifact. In one such worktree 22,224 directories were tracked and 161 held anything the index
//! did not list.
//!
//! The index already says what a tracked directory contains, and on APFS a directory's link
//! count is its number of entries plus two. So a tracked directory is **clean** — holds nothing
//! but its tracked entries, and therefore nothing untracked to classify — when both hold:
//!
//! - its link count is two more than the number of tracked names the index gives it, and
//! - it was last modified no later than the newest of those entries was written, as the index
//!   recorded it: a directory's `mtime` moves whenever an entry is added, removed or renamed,
//!   so a deletion balanced by an addition still shows.
//!
//! A clean directory costs one `lstat`. Everything else — a dirty directory's untracked entries,
//! the tracked directories themselves — goes through the same [`Visitor::decide`] the directory
//! walk uses, so the two walks answer every entry identically: a tracked `dist/` beside a
//! `package.json` is still found, and still refused by the tracked-files rail in `run.rs`. Only
//! the directories the index can vouch for go unlisted.
//!
//! What the rule can miss is a directory that, since the index last recorded its newest entry,
//! lost tracked entries and gained the same number of untracked ones, and then had a tracked entry
//! written again and staged — the count and the time both agree with the index, and the untracked
//! entries are not seen. That costs recall in that directory, never safety: nothing reaches a
//! removal without passing the classifier and every rail.
//!
//! Only on APFS, whose directory link counts count entries; anywhere else, and wherever git
//! cannot read the index, the work tree is walked as before.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use ignore::WalkState;
use rayon::prelude::*;

use super::visitor::Visitor;
use super::{Message, WalkFailure};

/// A git gitlink — a submodule — in the index's mode column.
const GITLINK: &[u8] = b"160000";

/// Whether `path` is the top of a work tree: it holds a `.git` directory, or the `.git` file of a
/// linked worktree or a submodule.
pub(super) fn is_work_tree(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join(".git")).is_ok()
}

/// Whether the filesystem under `path` counts a directory's entries in its link count.
#[cfg(target_vendor = "apple")]
pub(super) fn supported(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(raw) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `statfs` is all integers and a fixed-size name array, so a zeroed value is a valid
    // one, and the call only writes into it.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `raw` is NUL-terminated and outlives the call; `stats` is a valid out-pointer.
    if unsafe { libc::statfs(raw.as_ptr(), &raw mut stats) } != 0 {
        return false;
    }
    // SAFETY: the kernel NUL-terminates `f_fstypename`.
    let name = unsafe { std::ffi::CStr::from_ptr(stats.f_fstypename.as_ptr()) };
    name.to_bytes() == b"apfs"
}

/// Every other platform: no filesystem here is known to count entries in a link count.
#[cfg(not(target_vendor = "apple"))]
pub(super) fn supported(_path: &Path) -> bool {
    false
}

/// One tracked directory, as the index describes it.
#[derive(Debug, Default)]
struct Node {
    /// The directory, relative to the work tree.
    path: Vec<u8>,
    /// Every name the index gives this directory: files, links, submodules and directories.
    names: HashSet<Box<[u8]>>,
    /// How many of those names exist on disk, as far as the index knows.
    present: u64,
    /// Subdirectories with something present below them.
    subdirs: Vec<usize>,
    /// Submodules directly inside, which are work trees of their own.
    gitlinks: Vec<Box<[u8]>>,
    /// The newest moment, in nanoseconds, any direct entry was written as the index recorded it.
    newest: i128,
    /// The oldest such moment anywhere below, which is when this directory was being filled.
    oldest: i128,
}

/// A work tree's index as a tree of tracked directories, root first.
#[derive(Debug)]
struct Index {
    nodes: Vec<Node>,
}

/// One entry of `git ls-files -z -s -t --debug`.
struct Entry<'a> {
    tag: u8,
    mode: &'a [u8],
    path: &'a [u8],
    ctime: i128,
}

impl Index {
    /// Builds the tree from the listing, which git prints sorted by path.
    ///
    /// `None` when the listing is not in the shape expected, so a git whose output differs falls
    /// back to the directory walk rather than to a wrong model.
    fn parse(listing: &[u8]) -> Option<Self> {
        let mut nodes = vec![Node {
            oldest: i128::MAX,
            ..Node::default()
        }];
        let mut stack: Vec<usize> = vec![0];
        let mut previous: &[u8] = b"";
        for entry in entries(listing)? {
            // Unmerged paths appear once per stage; the name is one name.
            if entry.path == previous {
                continue;
            }
            previous = entry.path;
            let (dir, name) = match entry.path.iter().rposition(|byte| *byte == b'/') {
                Some(slash) => (&entry.path[..slash], &entry.path[slash + 1..]),
                None => (&b""[..], entry.path),
            };
            // Leave every open directory that is not an ancestor of this entry, then enter the
            // ones that are. Sorted input keeps each directory's entries contiguous.
            while stack.len() > 1 && !is_within(dir, &nodes[*stack.last()?].path) {
                let done = stack.pop()?;
                finish(&mut nodes, done, *stack.last()?);
            }
            let open = &nodes[*stack.last()?].path;
            let rest = if open.is_empty() {
                dir
            } else {
                dir.get(open.len() + 1..).unwrap_or_default()
            };
            let mut built = nodes[*stack.last()?].path.clone();
            for component in rest.split(|byte| *byte == b'/').filter(|part| !part.is_empty()) {
                let parent = *stack.last()?;
                nodes[parent].names.insert(component.into());
                if !built.is_empty() {
                    built.push(b'/');
                }
                built.extend_from_slice(component);
                nodes.push(Node {
                    path: built.clone(),
                    oldest: i128::MAX,
                    ..Node::default()
                });
                stack.push(nodes.len() - 1);
            }
            let parent = &mut nodes[*stack.last()?];
            parent.names.insert(name.into());
            // A skip-worktree entry is absent by design: a sparse checkout leaves it out.
            if entry.tag == b'S' {
                continue;
            }
            parent.present += 1;
            if entry.mode == GITLINK {
                parent.gitlinks.push(name.into());
            } else {
                parent.newest = parent.newest.max(entry.ctime);
                parent.oldest = parent.oldest.min(entry.ctime);
            }
        }
        while stack.len() > 1 {
            let done = stack.pop()?;
            finish(&mut nodes, done, *stack.last()?);
        }
        Some(Self { nodes })
    }
}

/// Whether `dir` is `ancestor` or below it.
fn is_within(dir: &[u8], ancestor: &[u8]) -> bool {
    ancestor.is_empty() || dir == ancestor || (dir.starts_with(ancestor) && dir.get(ancestor.len()) == Some(&b'/'))
}

/// Folds a finished directory into its parent: present only if something below it is.
fn finish(nodes: &mut [Node], done: usize, parent: usize) {
    let (present, oldest) = (nodes[done].present > 0, nodes[done].oldest);
    if !present {
        return;
    }
    let parent = &mut nodes[parent];
    parent.present += 1;
    parent.subdirs.push(done);
    parent.newest = parent.newest.max(oldest);
    parent.oldest = parent.oldest.min(oldest);
}

/// Splits the listing into entries. Each NUL ends one entry's header; the `--debug` lines that
/// follow it run up to the next entry's header.
fn entries(listing: &[u8]) -> Option<Vec<Entry<'_>>> {
    let mut parts = listing.split(|byte| *byte == 0);
    let mut header = parts.next()?;
    let mut found = Vec::new();
    for part in parts {
        let (debug, next) = match part.iter().rposition(|byte| *byte == b'\n') {
            Some(newline) => (&part[..newline], &part[newline + 1..]),
            None => (part, &b""[..]),
        };
        if !header.is_empty() {
            found.push(entry(header, debug)?);
        }
        header = next;
    }
    header.is_empty().then_some(found)
}

/// One `T MODE HASH STAGE\tPATH` header and its `ctime: S:N` line.
fn entry<'a>(header: &'a [u8], debug: &[u8]) -> Option<Entry<'a>> {
    let tab = header.iter().position(|byte| *byte == b'\t')?;
    let mut fields = header[..tab].split(|byte| *byte == b' ');
    let tag = *fields.next()?.first()?;
    let mode = fields.next()?;
    let ctime = debug.split(|byte| *byte == b'\n').find_map(|line| {
        let value = line.trim_ascii().strip_prefix(b"ctime: ")?;
        let colon = value.iter().position(|byte| *byte == b':')?;
        let seconds: i128 = std::str::from_utf8(&value[..colon]).ok()?.parse().ok()?;
        let nanos: i128 = std::str::from_utf8(&value[colon + 1..]).ok()?.parse().ok()?;
        Some(seconds * 1_000_000_000 + nanos)
    })?;
    Some(Entry {
        tag,
        mode,
        path: &header[tab + 1..],
        ctime,
    })
}

/// Walks every work tree in `work_trees` from its index.
///
/// Returns what the directory walk must still visit: untracked directories the visitor chose to
/// descend, and any work tree git could not describe. Work trees met along the way are sent back
/// as [`Message::Guide`] for the next round.
pub(super) fn walk(visitor: Visitor<'_>, work_trees: &[PathBuf], sender: &mpsc::Sender<Message>) -> Vec<PathBuf> {
    work_trees
        .par_iter()
        .flat_map_iter(|work_tree| {
            let index = crate::git::index_listing(work_tree).and_then(|listing| Index::parse(&listing));
            let Some(index) = index else {
                return vec![work_tree.clone()];
            };
            let Ok(metadata) = std::fs::symlink_metadata(work_tree) else {
                return Vec::new();
            };
            let tree = Tree {
                visitor,
                index: &index,
                top: work_tree,
                dev: std::os::unix::fs::MetadataExt::dev(&metadata),
                sender,
            };
            tree.visit(0, work_tree, &metadata)
        })
        .collect()
}

/// One work tree being walked from its index.
struct Tree<'a> {
    visitor: Visitor<'a>,
    index: &'a Index,
    top: &'a Path,
    dev: u64,
    sender: &'a mpsc::Sender<Message>,
}

impl Tree<'_> {
    /// Visits one tracked directory that the visitor has already decided to descend.
    fn visit(&self, node: usize, path: &Path, metadata: &std::fs::Metadata) -> Vec<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let model = &self.index.nodes[node];
        if metadata.dev() != self.dev && self.visitor.options.one_file_system {
            return Vec::new();
        }
        let modified = i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
        let clean = metadata.nlink().checked_sub(2) == Some(model.present) && modified <= model.newest;

        let mut descend = if clean { Vec::new() } else { self.untracked(model, path) };
        for gitlink in &model.gitlinks {
            descend.extend(self.offer(&path.join(OsStr::from_bytes(gitlink)), true, false));
        }
        let below: Vec<PathBuf> = model
            .subdirs
            .par_iter()
            .flat_map_iter(|child| {
                let child_path = self.top.join(OsStr::from_bytes(&self.index.nodes[*child].path));
                // Deleted since the index was written: there is nothing below to find.
                let Ok(metadata) = std::fs::symlink_metadata(&child_path) else {
                    return Vec::new();
                };
                if !metadata.is_dir() {
                    // A tracked directory replaced by something else is untracked content.
                    return self.offer(&child_path, false, metadata.file_type().is_symlink());
                }
                let name = child_path.file_name().map(OsStr::to_owned).unwrap_or_default();
                match self.visitor.decide(&child_path, &name, true, false, false, self.sender) {
                    WalkState::Continue => self.visit(*child, &child_path, &metadata),
                    _ => Vec::new(),
                }
            })
            .collect();
        descend.extend(below);
        descend
    }

    /// Lists a dirty directory and offers the visitor every entry the index does not account for.
    fn untracked(&self, model: &Node, path: &Path) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                let failure = WalkFailure {
                    path: Some(path.to_path_buf()),
                    message: error.to_string(),
                    transient: error.kind() == std::io::ErrorKind::Interrupted,
                    inaccessible: error.kind() == std::io::ErrorKind::PermissionDenied,
                };
                let _ = self.sender.send(Message::Failure(Box::new(failure)));
                return Vec::new();
            }
        };
        let descended: HashSet<&[u8]> = model
            .subdirs
            .iter()
            .filter_map(|child| last_component(&self.index.nodes[*child].path))
            .chain(model.gitlinks.iter().map(AsRef::as_ref))
            .collect();
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let bytes = name.as_bytes();
            if descended.contains(bytes) {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            // A tracked file is accounted for; a directory standing where the index has a file,
            // or a tracked directory with nothing of the index's left in it, is not.
            if model.names.contains(bytes) && !kind.is_dir() {
                continue;
            }
            found.extend(self.offer(&entry.path(), kind.is_dir(), kind.is_symlink()));
        }
        found
    }

    /// Hands one entry to the visitor, returning it when it is a directory to walk.
    fn offer(&self, path: &Path, is_dir: bool, is_symlink: bool) -> Vec<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let name = path.file_name().map(OsStr::to_owned).unwrap_or_default();
        if is_dir
            && self.visitor.options.one_file_system
            && std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.dev() != self.dev)
        {
            return Vec::new();
        }
        match self.visitor.decide(path, &name, is_dir, is_symlink, true, self.sender) {
            WalkState::Continue if is_dir => vec![path.to_path_buf()],
            _ => Vec::new(),
        }
    }
}

/// The last `/`-separated component of a relative path.
fn last_component(path: &[u8]) -> Option<&[u8]> {
    path.rsplit(|byte| *byte == b'/').next().filter(|part| !part.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(entries: &[(&str, &str, &str, i128)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, mode, path, ctime) in entries {
            out.extend_from_slice(
                format!("{tag} {mode} 0000000000000000000000000000000000000000 0\t{path}").as_bytes(),
            );
            out.push(0);
            out.extend_from_slice(
                format!(
                    "  ctime: {}:{}\n  mtime: 0:0\n  dev: 1\tino: 1\n  uid: 1\tgid: 1\n  size: 1\tflags: 0\n",
                    ctime / 1_000_000_000,
                    ctime % 1_000_000_000
                )
                .as_bytes(),
            );
        }
        out
    }

    #[test]
    fn should_build_the_tracked_tree_with_counts_and_times() {
        let raw = listing(&[
            ("H", "100644", "a-b/x", 5),
            ("H", "100644", "a.c", 7),
            ("H", "100644", "a/b/c.txt", 3),
            ("H", "100644", "a/d.txt", 9),
            ("H", "160000", "sub", 0),
            ("S", "100644", "sparse/gone.txt", 1),
            ("H", "100644", "z", 2),
        ]);
        let index = Index::parse(&raw).expect("the listing parses");
        let root = &index.nodes[0];
        // a-b, a.c, a, sub, sparse, z — `sparse` has nothing present, so it is a name and no more.
        assert_eq!(root.names.len(), 6);
        assert_eq!(root.present, 5);
        assert_eq!(root.gitlinks, vec![b"sub".to_vec().into_boxed_slice()]);
        let names: Vec<&[u8]> = root
            .subdirs
            .iter()
            .map(|child| index.nodes[*child].path.as_slice())
            .collect();
        assert_eq!(names, vec![b"a-b".as_slice(), b"a".as_slice()]);
        let a = &index.nodes[root.subdirs[1]];
        assert_eq!(a.present, 2, "b and d.txt");
        assert_eq!(a.newest, 9, "d.txt, or b's oldest entry, whichever is newer");
        assert_eq!(root.newest, 7, "a.c beats a's oldest entry (3) and a-b's (5)");
    }

    #[test]
    fn should_count_an_unmerged_path_once() {
        let raw = listing(&[
            ("M", "100644", "f", 1),
            ("M", "100644", "f", 1),
            ("M", "100644", "f", 1),
        ]);
        let index = Index::parse(&raw).expect("the listing parses");
        assert_eq!(index.nodes[0].present, 1);
    }

    #[test]
    fn should_refuse_a_listing_it_does_not_understand() {
        assert!(Index::parse(b"H 100644 abc 0\tf\0garbage").is_none());
    }
}
