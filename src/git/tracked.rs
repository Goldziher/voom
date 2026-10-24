//! Which candidate artifacts hold files a repository tracks.
//!
//! A marker proves an ecosystem; it does not prove that nobody committed the directory beside it.
//! A `dist/` holding a checked-in icon sprite is shaped exactly like a build's output, and a sweep
//! that took it left every checkout of that repository with deleted tracked files — work a person
//! wrote, recoverable only because git had a copy. The index is the authority on what was
//! committed, so it is asked, once per repository, about every candidate inside it.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one repository's index may take to answer. A local index answers in milliseconds;
/// the bound exists for a repository on a stalled network mount.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Pathspecs per `git ls-files`, keeping the command line well inside every platform's limit.
const CHUNK: usize = 256;

/// The candidates, of those given, that hold at least one file their repository tracks.
///
/// Each candidate is answered by the deepest of `repositories` it sits inside — a submodule or a
/// nested repository, not its parent. A candidate in no repository, or in one whose index git
/// could not read, is not in the result: the question has no answer there, and the walk's own
/// proof is all there is.
#[must_use]
pub fn holding_tracked_files(candidates: &[&Path], repositories: &[PathBuf]) -> HashSet<PathBuf> {
    use rayon::prelude::*;

    let known: HashSet<&Path> = repositories.iter().map(PathBuf::as_path).collect();
    let mut by_repository: HashMap<&Path, Vec<&Path>> = HashMap::new();
    for candidate in candidates {
        if let Some(repository) = candidate.ancestors().skip(1).find(|ancestor| known.contains(ancestor)) {
            by_repository.entry(repository).or_default().push(candidate);
        }
    }
    by_repository
        .into_par_iter()
        .flat_map_iter(|(repository, candidates)| tracked_in(repository, &candidates))
        .collect()
}

/// The candidates inside one repository that its index names files under.
fn tracked_in(repository: &Path, candidates: &[&Path]) -> Vec<PathBuf> {
    let mut held = Vec::new();
    for chunk in candidates.chunks(CHUNK) {
        let relative: Vec<(&Path, &Path)> = chunk
            .iter()
            .filter_map(|candidate| Some((*candidate, candidate.strip_prefix(repository).ok()?)))
            .collect();
        // `:(literal)` so a directory named `[abc]` or `*` is the directory and not a glob, and
        // `--error-unmatch` is deliberately absent: most candidates match nothing, which is the
        // answer being asked for.
        let mut args: Vec<OsString> = vec!["ls-files".into(), "-z".into(), "--".into()];
        args.extend(relative.iter().map(|(_, rel)| {
            let mut spec = OsString::from(":(literal)");
            spec.push(rel.as_os_str());
            spec
        }));
        let args: Vec<&std::ffi::OsStr> = args.iter().map(OsString::as_os_str).collect();
        let Some(listed) = super::run::git_stdout(repository, &args, TIMEOUT) else {
            continue;
        };
        let tracked: Vec<&Path> = listed
            .split('\0')
            .filter(|line| !line.is_empty())
            .map(Path::new)
            .collect();
        held.extend(
            relative
                .iter()
                .filter(|(_, rel)| tracked.iter().any(|file| file.starts_with(rel)))
                .map(|(candidate, _)| candidate.to_path_buf()),
        );
    }
    held
}

/// Every index entry of a work tree with its tag, mode and recorded `ctime`, as
/// `git ls-files -z -s -t --debug` prints them. `None` when git could not answer.
#[must_use]
pub fn index_listing(work_tree: &Path) -> Option<Vec<u8>> {
    super::run::git_bytes(work_tree, &["ls-files", "-z", "-s", "-t", "--debug"], TIMEOUT)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::testing::tree;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn should_name_a_candidate_holding_a_tracked_file_and_only_that_one() {
        let root = tree(&[
            "repo/web/package.json",
            "repo/web/dist/icons.svg",
            "repo/app/package.json",
            "repo/app/dist/bundle.js",
        ]);
        let repo = root.path().join("repo");
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["add", "web/package.json", "web/dist/icons.svg", "app/package.json"],
        );

        let committed = repo.join("web/dist");
        let built = repo.join("app/dist");
        let held = holding_tracked_files(&[&committed, &built], std::slice::from_ref(&repo));

        assert_eq!(
            held,
            HashSet::from([committed]),
            "only the directory git tracks a file in"
        );
    }

    #[test]
    fn should_say_nothing_about_a_candidate_outside_every_repository() {
        let root = tree(&["loose/dist/x.js"]);
        let candidate = root.path().join("loose/dist");
        assert!(holding_tracked_files(&[&candidate], &[]).is_empty());
    }

    #[test]
    fn should_treat_a_glob_shaped_name_literally() {
        let root = tree(&["repo/[ab]c/kept.txt", "repo/ac/x"]);
        let repo = root.path().join("repo");
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "ac/x"]);
        let candidate = repo.join("[ab]c");
        assert!(
            holding_tracked_files(&[&candidate], &[repo]).is_empty(),
            "`[ab]c` is not `ac`"
        );
    }
}
