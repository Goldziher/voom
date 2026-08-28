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
        Some(Command::Watch(args)) => watch(cli, args),
        None => prune(cli),
    }
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
    let options = cli.prune.to_run_options()?;
    let result = voom::run::run(&options).context("scanning")?;

    let mut out = anstream::stdout().lock();
    voom::cli::render(&result, &cli.prune, &mut out).context("writing the report")?;
    out.flush().context("flushing the report")?;

    Ok(result.exit_code(cli.prune.exit_code))
}
