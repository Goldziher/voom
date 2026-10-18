//! Checkouts whose git administration directory has been deleted.
//!
//! A linked worktree is a directory plus `<main>/.git/worktrees/<name>`, which holds its `HEAD`,
//! its index and its reflog. Delete the second (`git worktree prune` after the directory moved,
//! a `rm -rf` of `.git/worktrees`) and the first is a stranded copy of the tree: `git status`
//! fails in it, so nothing voom checks for ordinary worktrees can be checked. It still owns disk
//! and may hold the only copy of uncommitted work.
//!
//! voom therefore **reports such a checkout and keeps it by default**. Every proof the removal rules
//! rely on is gone with the administration: there is no `HEAD` to test for ancestry, no index to
//! compare, and no way to date the loss, so "nothing here is newer than the deletion" cannot be
//! established either. Explicit aggressive cleanup accepts that loss, rechecks the missing
//! administration and applies the ordinary deletion guard. See ADR 0016 and its successor.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{Outcome, RepositoryWorktrees, State, Worktree, WorktreeOptions};

/// A checkout and the repository its `.git` file says it belonged to.
pub(super) struct Orphan {
    /// The main checkout, derived from the gitdir path `<main>/.git/worktrees/<name>`.
    main: PathBuf,
    /// The administration directory that is gone.
    gitdir: PathBuf,
}

/// Recognises a checkout whose `.git` file names a missing `<store>/worktrees/<name>` directory.
///
/// Anything else that git cannot open — no `.git`, a `.git` directory, a gitdir that exists but is
/// broken — is not this case and yields `None`.
pub(super) fn orphan(work_tree: &Path) -> Option<Orphan> {
    let dot_git = work_tree.join(".git");
    if !std::fs::symlink_metadata(&dot_git).ok()?.file_type().is_file() {
        return None;
    }
    let contents = crate::io::read_capped_text(&dot_git, 4096)?;
    let named = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    let gitdir = if Path::new(named).is_absolute() {
        PathBuf::from(named)
    } else {
        work_tree.join(named)
    };
    if gitdir.exists() || gitdir.parent()?.file_name() != Some(OsStr::new("worktrees")) {
        return None;
    }
    let store = gitdir.parent()?.parent()?;
    let main = if store.file_name() == Some(OsStr::new(".git")) {
        store.parent()?
    } else {
        store
    };
    Some(Orphan {
        main: main.to_path_buf(),
        gitdir,
    })
}

/// Files the checkout under its repository, or under one of its own when the repository was
/// not otherwise examined (the main checkout is outside the sweep, or gone).
pub(super) fn attach(
    found: &mut Vec<RepositoryWorktrees>,
    checkout: &Path,
    orphan: &Orphan,
    roots: &[PathBuf],
    current: Option<&Path>,
    options: WorktreeOptions,
) {
    let canonical = checkout.canonicalize().unwrap_or_else(|_| checkout.to_path_buf());
    let mut state = if !roots
        .iter()
        .any(|root| canonical != *root && canonical.starts_with(root))
    {
        State::OutsideRoot
    } else if current.is_some_and(|cwd| cwd.starts_with(&canonical)) {
        State::Current
    } else {
        State::Dangling {
            gitdir: orphan.gitdir.clone(),
            bytes: 0,
        }
    };
    let outcome = if let State::Dangling { bytes, .. } = &mut state {
        if options.aggressive {
            let (outcome, measured) = remove(checkout, orphan, roots, options.dry_run);
            *bytes = measured;
            Some(outcome)
        } else {
            *bytes = crate::size::measure(&canonical);
            None
        }
    } else {
        None
    };
    let entry = Worktree {
        path: canonical,
        branch: None,
        state,
        outcome,
    };
    let main = orphan.main.canonicalize().unwrap_or_else(|_| orphan.main.clone());
    match found.iter_mut().find(|repository| repository.repository == main) {
        Some(repository) => {
            repository.worktrees.push(entry);
            repository.worktrees.sort_by(|left, right| left.path.cmp(&right.path));
        }
        None => found.push(RepositoryWorktrees {
            repository: main,
            default_branch: None,
            worktrees: vec![entry],
            error: None,
        }),
    }
}

fn remove(checkout: &Path, expected: &Orphan, roots: &[PathBuf], dry_run: bool) -> (Outcome, u64) {
    let guard = match crate::delete::Guard::new(roots, true) {
        Ok(guard) => guard,
        Err(error) => return (Outcome::Failed(format!("checking dangling checkout: {error}")), 0),
    };
    // The checkout may have been repaired while scanning. Never delete one whose administration
    // has appeared or whose gitdir changed since it was identified as dangling.
    if orphan(checkout).is_none_or(|now| now.gitdir != expected.gitdir) {
        return (
            Outcome::Failed("dangling checkout administration changed before removal".to_owned()),
            0,
        );
    }
    let removal = if dry_run {
        crate::delete::Removal::dry()
    } else {
        crate::delete::Removal::new()
    };
    let (outcome, bytes) = guard.reclaim(checkout, removal);
    let outcome = match outcome {
        crate::delete::Outcome::Removed => Outcome::Removed,
        crate::delete::Outcome::WouldRemove => Outcome::WouldRemove,
        outcome => Outcome::Failed(format!(
            "removing dangling checkout {}: {outcome:?}",
            checkout.display()
        )),
    };
    (outcome, bytes)
}
