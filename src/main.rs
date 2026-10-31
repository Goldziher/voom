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

/// Raises the process's soft descriptor limit to its hard limit, best effort.
///
/// voom holds a descriptor for each level of a tree it walks and spawns a process per repository.
/// The soft limit is routinely far below the hard one — macOS gives a process launched from the
/// GUI a soft limit of 256 against an effectively unlimited hard one — and a sweep of a deep or
/// repository-dense tree otherwise fails with `EMFILE`. Failing to raise it is not fatal: the
/// descriptor-aware default pool in the library keeps voom inside whatever limit it is given.
fn raise_descriptor_limit() {
    #[cfg(unix)]
    {
        // SAFETY: `limit` is a plain out-parameter; both calls only read or write it.
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return;
        }
        if limit.rlim_cur < limit.rlim_max {
            limit.rlim_cur = limit.rlim_max;
            // SAFETY: `limit` is this process's own limit, read back just above.
            unsafe {
                libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit);
            }
        }
    }
}

fn main() -> ExitCode {
    raise_descriptor_limit();
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
        Some(Command::ClaudePrune(args)) => claude_prune(args),
        Some(Command::DockerPrune(args)) => docker_prune_command(args),
        Some(Command::Reap(args)) => {
            let mut out = anstream::stdout().lock();
            args.run(&mut out)?;
            Ok(exit::SUCCESS)
        }
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

/// `voom docker-prune`: Docker's disk space, run on its own. Never fails on docker's account.
fn docker_prune_command(args: &voom::cli::DockerPruneArgs) -> anyhow::Result<i32> {
    let options = args.to_docker_options()?;
    let result = voom::docker::prune(&options);

    let mut out = anstream::stdout().lock();
    voom::cli::render_docker(&result, args, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;

    Ok(exit::SUCCESS)
}

fn stale_after(value: Option<&str>, aggressive: bool) -> anyhow::Result<Option<std::time::Duration>> {
    value
        .map(voom::policy::parse_duration)
        .transpose()
        .context("reading --stale-worktrees")
        .map(|age| age.or(aggressive.then_some(std::time::Duration::ZERO)))
}

/// `voom git-prune`: git's own housekeeping, run on its own.
fn git_prune(cli: &Cli, args: &voom::cli::GitPruneArgs) -> anyhow::Result<i32> {
    let options = args.to_git_options(&cli.prune)?;
    let result = voom::git::prune(&options).context("pruning repositories")?;

    let worktrees = if args.remove_merged_worktrees || args.aggressive {
        let repositories = voom::git::discover_repositories(&options);
        Some(voom::worktrees::prune(
            &repositories,
            &options.roots,
            voom::worktrees::WorktreeOptions {
                dry_run: args.dry_run,
                stale_after: stale_after(args.stale_worktrees.as_deref(), args.aggressive)?,
                github_merged: args.github_merged_worktrees,
                aggressive: args.aggressive,
            },
        ))
    } else {
        None
    };
    let mut out = anstream::stdout().lock();
    match args.format {
        voom::cli::Format::Human => {
            voom::cli::render_git(&result, args, &mut out).context("writing the report")?;
            if let Some(worktrees) = &worktrees {
                voom::worktrees::render_human(worktrees, cli.prune.verbose, &mut out)
                    .context("writing the worktree report")?;
            }
        }
        voom::cli::Format::Json => {
            let mut document = voom::git::document(&result);
            if let Some(worktrees) = &worktrees {
                document["worktrees"] = voom::worktrees::document(worktrees);
            }
            serde_json::to_writer_pretty(&mut out, &document).context("writing the report")?;
            writeln!(out)?;
        }
    }
    out.flush().context("flushing the report")?;

    Ok(if result.exit_code() == exit::SUCCESS {
        worktrees
            .as_ref()
            .map_or(exit::SUCCESS, voom::worktrees::WorktreePruneResult::exit_code)
    } else {
        result.exit_code()
    })
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
        voom::cli::render_caches(&mut out, cli.prune.verbose, cli.prune.sizes).context("writing the cache table")?;
        return Ok(exit::SUCCESS);
    }

    let options = cli.prune.to_run_options()?;
    let detach = !options.dry_run && !cli.prune.wait && std::env::var_os("VOOM_WAIT").is_none_or(|value| value != "1");
    if detach {
        start_backlog_reaper()?;
        report_reaped()?;
        voom::delete::detach_removals(true);
    }
    let code = prune_stages(cli, &options)?;
    if detach {
        start_reaper()?;
    }
    Ok(code)
}

/// Drains what an interrupted earlier run left in the trash, before this run adds to it.
fn start_backlog_reaper() -> anyhow::Result<()> {
    match voom::delete::spawn_reaper_for_pending() {
        Ok(Some(pid)) => writeln!(
            anstream::stderr(),
            "voom: freeing leftovers from an earlier run in the background (reaper pid {pid})"
        )?,
        Ok(None) => {}
        Err(error) => writeln!(
            anstream::stderr(),
            "voom: could not start the trash reaper for leftovers ({error}); `voom reap` frees it"
        )?,
    }
    Ok(())
}

/// Says what background reapers freed since the last run collected their ledger.
fn report_reaped() -> anyhow::Result<()> {
    let Some(dir) = voom::delete::trash_location() else {
        return Ok(());
    };
    let freed = voom::delete::collect_ledger(&dir);
    if freed != voom::delete::Reaped::default() {
        voom::cli::write_reap_summary(&freed, &dir, &mut anstream::stderr())?;
    }
    Ok(())
}

/// Starts the background reaper for what this run detached, and says so.
fn start_reaper() -> anyhow::Result<()> {
    let detached = voom::delete::detached_count();
    match voom::delete::spawn_reaper() {
        Ok(Some(pid)) => writeln!(
            anstream::stderr(),
            "voom: freeing {detached} detached {} in the background (reaper pid {pid}); `voom reap` waits for it",
            if detached == 1 { "artifact" } else { "artifacts" },
        )?,
        Ok(None) => {}
        Err(error) => writeln!(
            anstream::stderr(),
            "voom: could not start the background reaper ({error}); `voom reap` frees the trash"
        )?,
    }
    Ok(())
}

/// Every stage of a sweep: the walk, Bazel, Docker and worktrees, and their reports.
fn prune_stages(cli: &Cli, options: &voom::run::RunOptions) -> anyhow::Result<i32> {
    let options = options.clone();

    // Bazel's housekeeping is machine-global and shares nothing with the sweep, so the two always
    // run side by side. Merged-worktree removal is the one thing that feeds it — a removed
    // worktree's output base is then an orphan in the same run — and that is answered by a
    // second, cheap pass afterwards rather than by holding the whole stage back (see below).
    //
    // Docker is just as independent, and a prune is mostly waiting on a daemon, so it overlaps
    // with everything.
    let (result, mut bazel, docker) = std::thread::scope(|scope| {
        let early = Some(scope.spawn(|| bazel_prune(cli, &options)));
        let docker = scope.spawn(|| docker_prune(cli, &options));
        let result = voom::run::run(&options).context("scanning");
        // The sweep is done; Bazel's removals may not be, and they can be most of a real run.
        let waiting = (!options.dry_run && early.as_ref().is_some_and(|handle| !handle.is_finished()))
            .then(|| voom::run::removal_progress(options.progress))
            .flatten();
        let joined = (result, early.map(std::thread::ScopedJoinHandle::join), docker.join());
        if let Some(waiting) = waiting {
            waiting.finish_and_clear();
        }
        joined
    });
    let result = result?;
    let early_bazel = match bazel.take() {
        Some(joined) => Some(joined.map_err(|_| anyhow::anyhow!("the bazel housekeeping panicked"))??),
        None => None,
    };
    let docker = docker_found(docker);
    let json = matches!(cli.prune.format, voom::cli::Format::Json);

    // Human output streams each stage as it finishes; JSON waits for every stage so that stdout
    // is one document holding all of them, rather than the sweep alone with the rest reduced to
    // summary lines on stderr — which is where a 200 GB Bazel finding used to hide.
    if !json {
        let mut out = anstream::stdout().lock();
        voom::cli::render(&result, &cli.prune, &mut out).context("writing the report")?;
        out.flush().context("flushing the report")?;
    }

    let worktrees = if cli.prune.remove_merged_worktrees || cli.prune.aggressive {
        merged_worktrees(cli, &result.repositories, &options.roots)?
    } else {
        None
    };
    if let (false, Some(worktrees)) = (json, &worktrees) {
        let mut out = anstream::stdout().lock();
        writeln!(out)?;
        voom::worktrees::render_human(worktrees, cli.prune.verbose, &mut out).context("writing the worktree report")?;
        out.flush().context("flushing the worktree report")?;
    }
    let mut bazel = match early_bazel {
        Some(bazel) => bazel,
        None => bazel_prune(cli, &options)?,
    };
    // Worktrees this run removed have just orphaned their output bases. A second pass reads
    // only each base's marker and server pid, and takes what the first could not have.
    let removed_worktrees = worktrees
        .as_ref()
        .is_some_and(|worktrees| !worktrees.dry_run && worktrees.totals().removed > 0);
    if removed_worktrees
        && let Some(first) = bazel.as_mut()
        && let Some(second) = bazel_prune(cli, &options)?
    {
        first.absorb(second);
    }

    if json {
        write_json_document(&result, bazel.as_ref(), docker.as_ref(), worktrees.as_ref())?;
    } else {
        report_bazel(bazel.as_ref())?;
        if let Some(docker) = &docker {
            report_docker(docker)?;
        }
    }

    Ok(first_failure(&result, cli, worktrees.as_ref(), bazel.as_ref()))
}

/// The docker stage's finding, with its failure reported on stderr rather than propagated.
fn docker_found(
    docker: std::thread::Result<anyhow::Result<Option<voom::docker::DockerPruneResult>>>,
) -> Option<voom::docker::DockerPruneResult> {
    // A docker problem is a note and nothing more; it must not fail the sweep. A daemon that is
    // simply down is a `Skip` inside the result and is rendered with the docker block; a genuine
    // error or a panic — for instance resolving the user config — is said aloud rather than
    // dropped, so a stage that never ran is never silent.
    match docker {
        Ok(Ok(found)) => found,
        Ok(Err(error)) => {
            eprintln!("voom: docker: {error}");
            None
        }
        Err(_) => {
            eprintln!("voom: docker: the docker stage panicked");
            None
        }
    }
}

/// Writes the one JSON document that holds every stage.
fn write_json_document(
    result: &voom::report::RunResult,
    bazel: Option<&voom::bazel::BazelPruneResult>,
    docker: Option<&voom::docker::DockerPruneResult>,
    worktrees: Option<&voom::worktrees::WorktreePruneResult>,
) -> anyhow::Result<()> {
    let mut document = voom::report::json::document(result);
    document["bazel"] = bazel.map_or(serde_json::Value::Null, voom::bazel::document);
    document["docker"] = docker.map_or(serde_json::Value::Null, voom::docker::document);
    document["worktrees"] = worktrees.map_or(serde_json::Value::Null, voom::worktrees::document);
    let mut out = anstream::stdout().lock();
    serde_json::to_writer_pretty(&mut out, &document).context("writing the report")?;
    writeln!(out)?;
    out.flush().context("flushing the report")?;
    Ok(())
}

/// The first non-success exit code among the sweep, the worktrees and Bazel.
fn first_failure(
    result: &voom::report::RunResult,
    cli: &Cli,
    worktrees: Option<&voom::worktrees::WorktreePruneResult>,
    bazel: Option<&voom::bazel::BazelPruneResult>,
) -> i32 {
    let code = result.exit_code(cli.prune.exit_code);
    let worktree_code = worktrees.map_or(exit::SUCCESS, voom::worktrees::WorktreePruneResult::exit_code);
    let bazel_code = bazel.map_or(exit::SUCCESS, voom::bazel::BazelPruneResult::exit_code);
    [code, worktree_code, bazel_code]
        .into_iter()
        .find(|code| *code != exit::SUCCESS)
        .unwrap_or(exit::SUCCESS)
}

/// Docker's housekeeping, when `--docker` or `[docker] enabled` asked for it.
fn docker_prune(
    cli: &Cli,
    run_options: &voom::run::RunOptions,
) -> anyhow::Result<Option<voom::docker::DockerPruneResult>> {
    let Some(resolved) = machine_config(run_options)? else {
        return Ok(None);
    };
    Ok(cli
        .prune
        .docker_options(&resolved)
        .map(|options| voom::docker::prune(&options)))
}

/// The machine-global configuration, decided once by the first scan root. `None` when there is no
/// root.
fn machine_config(
    run_options: &voom::run::RunOptions,
) -> anyhow::Result<Option<std::sync::Arc<voom::config::Resolved>>> {
    let Some(root) = run_options.roots.first() else {
        return Ok(None);
    };
    voom::run::resolver_for(root, run_options)
        .and_then(|resolver| resolver.root_config())
        .map(Some)
        .context("resolving configuration")
}

/// Prints the Docker stage after the other reports. Silent when docker had nothing to say; a skip
/// is one line on stderr. JSON runs carry the stage in the single document instead.
fn report_docker(result: &voom::docker::DockerPruneResult) -> anyhow::Result<()> {
    if let Some(skip) = &result.skipped {
        writeln!(anstream::stderr(), "voom: docker: {} (skipped)", skip.reason)?;
        return Ok(());
    }
    if !result.has_news() {
        return Ok(());
    }
    let mut out = anstream::stdout().lock();
    writeln!(out)?;
    voom::docker::render_human(result, &mut out).context("writing the docker report")?;
    out.flush().context("flushing the docker report")?;
    Ok(())
}

/// Bazel's own housekeeping: stale output bases and caches, or everything under
/// `--clear-caches`. `None` when the configuration turned it off.
fn bazel_prune(
    cli: &Cli,
    run_options: &voom::run::RunOptions,
) -> anyhow::Result<Option<voom::bazel::BazelPruneResult>> {
    // Machine-global, so decided once by the configuration at the first scan root.
    let Some(resolved) = machine_config(run_options)? else {
        return Ok(None);
    };
    let Some(mut options) = cli.prune.bazel_options(&resolved) else {
        return Ok(None);
    };
    options.scope = Some(
        run_options
            .roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .collect(),
    );
    voom::bazel::prune(&options)
        .context("pruning bazel output bases")
        .map(Some)
}

/// Prints the Bazel stage after the sweep's report. Silent when there was nothing to do. JSON
/// runs carry the stage in the single document instead.
fn report_bazel(result: Option<&voom::bazel::BazelPruneResult>) -> anyhow::Result<()> {
    let Some(result) = result else {
        return Ok(());
    };
    let totals = result.totals();
    if totals.removed == 0 && totals.refused == 0 {
        return Ok(());
    }
    let mut out = anstream::stdout().lock();
    writeln!(out)?;
    voom::bazel::render_human(result, &mut out).context("writing the bazel report")?;
    out.flush().context("flushing the bazel report")?;
    Ok(())
}

/// `--remove-merged-worktrees` after a sweep. `None` when the walk passed no repository at all,
/// which is said on stderr.
fn merged_worktrees(
    cli: &Cli,
    repositories: &[std::path::PathBuf],
    roots: &[std::path::PathBuf],
) -> anyhow::Result<Option<voom::worktrees::WorktreePruneResult>> {
    if repositories.is_empty() {
        // Not "0 removed": the walk passed no repository at all, which is a different claim. An
        // `--exclude` covering `.git` is the usual cause, since repositories are found by it.
        writeln!(
            anstream::stderr(),
            "voom: worktrees: the walk found no git repositories, so none were examined \
             (an --exclude matching `.git` hides them)"
        )?;
        return Ok(None);
    }
    let result = voom::worktrees::prune(
        repositories,
        roots,
        voom::worktrees::WorktreeOptions {
            dry_run: cli.prune.dry_run,
            github_merged: cli.prune.github_merged_worktrees,
            stale_after: stale_after(cli.prune.stale_worktrees.as_deref(), cli.prune.aggressive)?,
            aggressive: cli.prune.aggressive,
        },
    );
    Ok(Some(result))
}
