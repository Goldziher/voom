//! The `voom` binary.
//!
//! Deliberately thin: parse, dispatch, add context, map errors to exit codes. Everything
//! testable lives in the library so the catalog, classifier, policy engine and deleter can be
//! exercised without spawning a process.

use std::io::Write;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use voom::cli::{Cli, Command, ConfigAction};
use voom::report::exit;

fn main() -> ExitCode {
    let cli = Cli::parse();
    anstream::ColorChoice::write_global(cli.prune.color.into());

    match dispatch(&cli) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            // Diagnostics on stderr, results on stdout, so `voom ~ --format json | jq` works
            // without filtering (ADR 0007).
            let _ = writeln!(anstream::stderr(), "voom: {error:#}");
            ExitCode::from(u8::try_from(exit::USAGE).unwrap_or(2))
        }
    }
}

fn dispatch(cli: &Cli) -> anyhow::Result<i32> {
    match &cli.command {
        Some(Command::Catalog) => {
            let mut out = anstream::stdout().lock();
            voom::cli::render_catalog(&mut out).context("writing the catalog")?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Config {
            action: ConfigAction::Show { path },
        }) => {
            let mut out = anstream::stdout().lock();
            voom::cli::render_config(path, &cli.prune, &mut out).context("resolving configuration")?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Suggest(args)) => {
            let suggestions = voom::suggest::suggest(&args.paths, args.one_file_system, args.jobs);
            let mut out = anstream::stdout().lock();
            voom::cli::render_suggestions(&suggestions, &mut out).context("writing suggestions")?;
            Ok(exit::SUCCESS)
        }
        Some(Command::Watch(args)) => watch(cli, args),
        Some(Command::GitPrune(args)) => git_prune(cli, args),
        Some(Command::BazelPrune(args)) => bazel_prune(args),
        Some(Command::ClaudePrune(args)) => claude_prune(args),
        None => prune(cli),
    }
}

/// `voom claude-prune`: Claude Code job scratch, run on its own.
fn claude_prune(args: &voom::cli::ClaudePruneArgs) -> anyhow::Result<i32> {
    let options = args.to_claude_options()?;
    let result = voom::claude::prune(&options).context("pruning job scratch")?;

    let mut out = anstream::stdout().lock();
    voom::cli::render_claude(&result, args, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;

    Ok(result.exit_code())
}

/// `voom bazel-prune`: orphaned Bazel output bases, run on their own.
fn bazel_prune(args: &voom::cli::BazelPruneArgs) -> anyhow::Result<i32> {
    let options = args.to_bazel_options()?;
    let result = voom::bazel::prune(&options).context("pruning output bases")?;

    let mut out = anstream::stdout().lock();
    voom::cli::render_bazel(&result, args, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;

    Ok(result.exit_code())
}

/// `voom git-prune`: git's own housekeeping, run on its own.
fn git_prune(cli: &Cli, args: &voom::cli::GitPruneArgs) -> anyhow::Result<i32> {
    let options = args.to_git_options(&cli.prune)?;
    let result = voom::git::prune(&options).context("pruning repositories")?;

    let mut out = anstream::stdout().lock();
    voom::cli::render_git(&result, args, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;
    drop(out);

    if args.remove_merged_worktrees {
        let repositories = voom::git::discover_repositories(&options);
        let worktrees = voom::worktrees::prune(
            &repositories,
            &options.roots,
            voom::worktrees::WorktreeOptions { dry_run: args.dry_run },
        );
        let mut out = anstream::stdout().lock();
        match args.format {
            voom::cli::Format::Human => voom::worktrees::render_human(&worktrees, &mut out),
            voom::cli::Format::Json => voom::worktrees::render_json(&worktrees, &mut out),
        }
        .context("writing the worktree report")?;
        out.flush().context("flushing the worktree report")?;
        if result.exit_code() == exit::SUCCESS {
            return Ok(worktrees.exit_code());
        }
    }

    Ok(result.exit_code())
}

fn watch(cli: &Cli, args: &voom::cli::WatchArgs) -> anyhow::Result<i32> {
    let options = args.to_run_options(&cli.prune)?;
    let watch_options = args.to_watch_options()?;

    let mut out = anstream::stdout().lock();
    writeln!(
        out,
        "voom: watching {} — quiet period {:?}, debounce {:?}. Ctrl-C to stop.",
        options
            .roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        watch_options.quiet_period,
        watch_options.debounce
    )?;
    out.flush()?;

    voom::watch::watch(&options, &watch_options, |result| {
        let mut out = anstream::stdout().lock();
        voom::cli::render(result, &cli.prune, &mut out)?;
        out.flush()
    })
    .context("watching")?;

    Ok(exit::SUCCESS)
}

fn prune(cli: &Cli) -> anyhow::Result<i32> {
    if cli.prune.list_caches {
        let mut out = anstream::stdout().lock();
        voom::cli::render_caches(&mut out, cli.prune.verbose).context("writing the cache table")?;
        return Ok(exit::SUCCESS);
    }

    let options = cli.prune.to_run_options()?;
    let result = voom::run::run(&options).context("scanning")?;

    let mut out = anstream::stdout().lock();
    voom::cli::render(&result, &cli.prune, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;
    drop(out);

    // Before Bazel: a removed worktree's output base is then an orphan in the same run.
    let worktree_code = if cli.prune.remove_merged_worktrees {
        merged_worktrees(cli, &result.repositories, &options.roots)?
    } else {
        exit::SUCCESS
    };
    let bazel_code = bazel_housekeeping(cli, &options)?;
    let code = result.exit_code(cli.prune.exit_code);
    Ok([code, worktree_code, bazel_code]
        .into_iter()
        .find(|code| *code != exit::SUCCESS)
        .unwrap_or(exit::SUCCESS))
}

/// Bazel's own housekeeping after a sweep: stale output bases and caches, or everything under
/// `--clear-caches`. Silent when there was nothing to do; JSON runs get a one-line summary on
/// stderr so stdout stays a single document.
fn bazel_housekeeping(cli: &Cli, run_options: &voom::run::RunOptions) -> anyhow::Result<i32> {
    // Machine-global, so decided once by the configuration at the first scan root.
    let Some(root) = run_options.roots.first() else {
        return Ok(exit::SUCCESS);
    };
    let resolved = voom::run::resolver_for(root, run_options)
        .and_then(|resolver| resolver.root_config())
        .context("resolving configuration")?;
    let Some(options) = cli.prune.bazel_options(&resolved) else {
        return Ok(exit::SUCCESS);
    };
    let result = voom::bazel::prune(&options).context("pruning bazel output bases")?;
    let totals = result.totals();
    if totals.removed == 0 && totals.refused == 0 {
        return Ok(result.exit_code());
    }

    match cli.prune.format {
        voom::cli::Format::Human => {
            let mut out = anstream::stdout().lock();
            writeln!(out)?;
            voom::bazel::render_human(&result, &mut out).context("writing the bazel report")?;
            out.flush().context("flushing the bazel report")?;
        }
        voom::cli::Format::Json => {
            writeln!(
                anstream::stderr(),
                "voom: bazel: {} removed, {} refused ({} bytes{})",
                totals.removed,
                totals.refused,
                totals.bytes,
                if result.dry_run { ", dry run" } else { "" }
            )?;
        }
    }
    Ok(result.exit_code())
}

/// `--remove-merged-worktrees` after a sweep: human output follows the sweep report, JSON gets a
/// one-line summary on stderr so stdout stays a single document.
fn merged_worktrees(
    cli: &Cli,
    repositories: &[std::path::PathBuf],
    roots: &[std::path::PathBuf],
) -> anyhow::Result<i32> {
    if repositories.is_empty() {
        // Not "0 removed": the walk passed no repository at all, which is a different claim. An
        // `--exclude` covering `.git` is the usual cause, since repositories are found by it.
        writeln!(
            anstream::stderr(),
            "voom: worktrees: the walk found no git repositories, so none were examined \
             (an --exclude matching `.git` hides them)"
        )?;
        return Ok(exit::SUCCESS);
    }
    let result = voom::worktrees::prune(
        repositories,
        roots,
        voom::worktrees::WorktreeOptions {
            dry_run: cli.prune.dry_run,
        },
    );
    match cli.prune.format {
        voom::cli::Format::Human => {
            let mut out = anstream::stdout().lock();
            writeln!(out)?;
            voom::worktrees::render_human(&result, &mut out).context("writing the worktree report")?;
            out.flush().context("flushing the worktree report")?;
        }
        voom::cli::Format::Json => {
            let totals = result.totals();
            writeln!(
                anstream::stderr(),
                "voom: worktrees: {} removed, {} merged with local changes, {} failed{}",
                totals.removed,
                totals.local_changes,
                totals.failed,
                if result.dry_run { " (dry run)" } else { "" }
            )?;
        }
    }
    Ok(result.exit_code())
}
