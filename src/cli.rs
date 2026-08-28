//! Argument parsing and command dispatch.

use std::io;
use std::path::{Path, PathBuf};

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::catalog::CATALOG;
use crate::config::resolve::Flags;
use crate::error::Result;
use crate::policy::{KeepPolicy, parse_duration, parse_size};
use crate::report::human::HumanOptions;
use crate::report::{RunResult, human, json};
use crate::run::RunOptions;
use crate::watch::WatchOptions;

/// Fast, safe, parallel build-artifact pruning across 24 language ecosystems.
///
/// voom deletes by default. `--dry-run` previews. There is no confirmation prompt — the safety
/// comes from marker anchoring and the rails, not from a question you would learn to click
/// through.
#[derive(Debug, Parser)]
#[command(name = "voom", version, about, long_about = None, args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// What to do instead of sweeping.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Sweep arguments, used when no subcommand is given.
    #[command(flatten)]
    pub prune: PruneArgs,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the built-in ecosystem catalog.
    ///
    /// This is generated from the same table the classifier reads, so it cannot drift from
    /// what voom will actually do.
    Catalog,
    /// Inspect configuration.
    Config {
        /// What to show.
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Keep a tree pruned continuously as you work.
    Watch(WatchArgs),
}

/// `voom config` actions.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the fully merged, resolved configuration with the file each value came from.
    Show {
        /// The tree whose configuration to resolve.
        #[arg(value_name = "PATH", default_value = ".")]
        path: PathBuf,
    },
}

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Colored, sorted, human-readable.
    Human,
    /// A single JSON object with a stable, versioned shape.
    Json,
}

/// When to colorize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Color {
    /// Colorize when stdout is a terminal, honouring `NO_COLOR` and `CLICOLOR_FORCE`.
    Auto,
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
}

impl From<Color> for anstream::ColorChoice {
    fn from(color: Color) -> Self {
        match color {
            Color::Auto => Self::Auto,
            Color::Always => Self::Always,
            Color::Never => Self::Never,
        }
    }
}

/// Everything a sweep takes.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these are command-line flags, which are bools by nature"
)]
#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Trees to sweep.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Report what would be removed, without touching anything.
    #[arg(short = 'n', long, help_heading = "Behaviour")]
    pub dry_run: bool,

    /// Print the per-artifact report. This is the default; the flag is accepted so the
    /// published git hooks can name it explicitly.
    #[arg(long, help_heading = "Behaviour")]
    pub report: bool,

    /// Exit 3 when a dry run finds artifacts, so a hook can fail on findings.
    #[arg(long, help_heading = "Behaviour")]
    pub exit_code: bool,

    /// Retry a failed removal, repairing permissions inside the artifact first.
    ///
    /// Clears read-only bits, and on macOS the user-immutable flag, on paths *inside* an
    /// artifact that has already passed every safety rail — then retries with a short backoff.
    /// It never relaxes a rail, never follows a symlink, and never touches anything outside the
    /// artifact, including its parent directory: an artifact held by a read-only parent is
    /// reported, not forced. Retries add up to about a second per artifact. No effect under
    /// `--dry-run`.
    ///
    /// Filed under Behaviour rather than Safety deliberately — it is not one of the protections.
    #[arg(long, help_heading = "Behaviour")]
    pub force: bool,

    /// Explain every candidate that was passed over.
    #[arg(short, long, help_heading = "Output")]
    pub verbose: bool,

    /// Print only the totals footer.
    #[arg(long, help_heading = "Output")]
    pub summary: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = Format::Human, help_heading = "Output")]
    pub format: Format,

    /// When to colorize output.
    #[arg(long, value_enum, default_value_t = Color::Auto, help_heading = "Output")]
    pub color: Color,

    /// Worker threads. Lower this on spinning disks and network filesystems.
    #[arg(short = 'j', long, value_name = "N", help_heading = "Behaviour")]
    pub jobs: Option<usize>,

    /// Stay on the scan root's filesystem.
    ///
    /// `require_equals` is what makes `voom --one-file-system ~` work. Without it an optional
    /// value is greedy, so clap reads the path that follows as the flag's value and rejects the
    /// run — on a safety flag, in the ordinary invocation.
    #[arg(
        long,
        value_name = "BOOL",
        num_args = 0..=1,
        require_equals = true,
        default_value_t = true,
        default_missing_value = "true",
        action = ArgAction::Set,
        help_heading = "Safety"
    )]
    pub one_file_system: bool,

    /// Never remove an artifact modified more recently than this, e.g. `7d`.
    #[arg(long, value_name = "DURATION", help_heading = "Keep policies")]
    pub min_age: Option<String>,

    /// Skip artifacts smaller than this, e.g. `1MB`.
    #[arg(long, value_name = "SIZE", help_heading = "Keep policies")]
    pub min_size: Option<String>,

    /// Skip artifacts larger than this, e.g. `50GB`.
    #[arg(long, value_name = "SIZE", help_heading = "Keep policies")]
    pub max_size: Option<String>,

    /// Only sweep these ecosystems. Repeatable.
    #[arg(short = 'e', long, value_name = "ID", help_heading = "Selection")]
    pub ecosystem: Vec<String>,

    /// Turn on an off-by-default artifact, e.g. `python.venv`. Repeatable.
    #[arg(long, value_name = "SPEC", help_heading = "Selection")]
    pub enable: Vec<String>,

    /// Turn off an ecosystem or artifact. Repeatable.
    #[arg(long, value_name = "SPEC", help_heading = "Selection")]
    pub disable: Vec<String>,

    /// Never scan these paths. Repeatable.
    #[arg(long, value_name = "GLOB", help_heading = "Selection")]
    pub exclude: Vec<String>,

    /// Also sweep machine-global tool caches and installed toolchains.
    ///
    /// Skipped by default: `~/.cargo/registry`, `~/.pyenv`, `~/google-cloud-sdk` and friends are
    /// shared across every project or are installed programs, not this tree's build output.
    #[arg(long, help_heading = "Selection")]
    pub caches: bool,

    /// Use this configuration file instead of the discovered hierarchy.
    #[arg(long, value_name = "PATH", help_heading = "Selection")]
    pub config: Option<PathBuf>,
}

/// Watch-mode arguments.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Trees to keep pruned.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Coalesce filesystem events over this window.
    #[arg(long, value_name = "DURATION", default_value = "5s")]
    pub debounce: String,

    /// Only remove an artifact after its subtree has been idle this long.
    #[arg(long, value_name = "DURATION", default_value = "60s")]
    pub quiet_period: String,
}

impl WatchArgs {
    /// The pipeline options for a watch, taking the sweep flags from the top-level arguments.
    ///
    /// # Errors
    ///
    /// Whatever [`PruneArgs::flags`] rejects.
    pub fn to_run_options(&self, prune: &PruneArgs) -> Result<RunOptions> {
        let mut options = prune.to_run_options()?;
        options.roots.clone_from(&self.paths);
        Ok(options)
    }

    /// The debounce and quiet-period windows.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::InvalidDuration`] for an unparseable window.
    pub fn to_watch_options(&self) -> Result<WatchOptions> {
        Ok(WatchOptions {
            debounce: parse_duration(&self.debounce)?,
            quiet_period: parse_duration(&self.quiet_period)?,
        })
    }
}

impl PruneArgs {
    /// Turns flags into the overrides that sit above every configuration layer.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] or [`Error::InvalidSize`] for an unparseable keep value.
    pub fn flags(&self) -> Result<Flags> {
        Ok(Flags {
            ecosystems: (!self.ecosystem.is_empty()).then(|| self.ecosystem.clone()),
            enable: self.enable.clone(),
            disable: self.disable.clone(),
            keep: self.keep()?,
            exclude: self.exclude.clone(),
        })
    }

    /// Turns flags into keep policies.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] or [`Error::InvalidSize`] for an unparseable value.
    pub fn keep(&self) -> Result<KeepPolicy> {
        Ok(KeepPolicy {
            min_age: self.min_age.as_deref().map(parse_duration).transpose()?,
            min_size: self.min_size.as_deref().map(parse_size).transpose()?,
            max_size: self.max_size.as_deref().map(parse_size).transpose()?,
        })
    }

    /// Assembles the run.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::flags`] rejects.
    pub fn to_run_options(&self) -> Result<RunOptions> {
        Ok(RunOptions {
            roots: self.paths.clone(),
            dry_run: self.dry_run,
            jobs: self.jobs,
            one_file_system: self.one_file_system,
            force: self.force,
            caches: self.caches,
            // JSON always carries skip detail — a machine consumer has no `--verbose` to reach
            // for and paying for the detail is cheaper than a second run.
            verbose: self.verbose || self.format == Format::Json,
            config: self.config.clone(),
            flags: self.flags()?,
            // A spinner is only useful for a human watching a scan scroll past; JSON and
            // summary output would only be interrupted by it.
            progress: self.format == Format::Human && !self.summary,
        })
    }
}

/// Renders a finished run in the requested format.
///
/// # Errors
///
/// Propagates write failures.
pub fn render(result: &RunResult, args: &PruneArgs, out: &mut impl io::Write) -> io::Result<()> {
    match args.format {
        Format::Human => {
            let options = HumanOptions {
                verbose: args.verbose,
                summary_only: args.summary,
            };
            human::render(result, options, out)
        }
        Format::Json => json::render(result, out),
    }
}

/// Prints the built-in catalog.
///
/// # Errors
///
/// Propagates write failures.
pub fn render_catalog(out: &mut impl io::Write) -> io::Result<()> {
    use owo_colors::OwoColorize;

    for ecosystem in CATALOG {
        writeln!(
            out,
            "{} {}",
            ecosystem.name.bold(),
            format_args!("({})", ecosystem.id).dimmed()
        )?;
        writeln!(out, "  markers: {}", ecosystem.markers.join(", "))?;
        let anchor = match ecosystem.anchor {
            crate::catalog::Anchor::Sibling => "sibling".to_owned(),
            crate::catalog::Anchor::Ancestor(levels) => format!("ancestor({levels})"),
        };
        writeln!(out, "  anchor:  {anchor}")?;
        for artifact in ecosystem.artifacts {
            let state = if artifact.default_on { "on " } else { "off" };
            write!(
                out,
                "  {state}  {:<24} {}.{}",
                artifact.path,
                ecosystem.id,
                artifact.slug()
            )?;
            match artifact.note {
                Some(note) => writeln!(out, "\n          {}", note.dimmed())?,
                None => writeln!(out)?,
            }
        }
        writeln!(out)?;
    }
    writeln!(
        out,
        "{}",
        "`off` artifacts need an explicit opt-in: --enable <spec>, or in voom.toml.".dimmed()
    )
}

/// Prints the fully merged, resolved configuration with the files it came from.
///
/// A hierarchical merge nobody can inspect is a hierarchical merge nobody can trust — this is
/// what a user reaches for when asking "why did it delete that?" (ADR 0004).
///
/// # Errors
///
/// [`crate::error::Error::Config`] if a configuration file is broken, or a write failure.
pub fn render_config(root: &Path, args: &PruneArgs, out: &mut impl io::Write) -> Result<()> {
    use owo_colors::OwoColorize;

    let mut options = args.to_run_options()?;
    options.roots = vec![root.to_path_buf()];
    let merged = crate::run::resolver_for(root, &options)?.root_config()?;

    let write = |out: &mut dyn io::Write| -> io::Result<()> {
        writeln!(out, "{}", "sources (ascending precedence)".bold())?;
        if merged.sources.is_empty() {
            writeln!(out, "  (none — built-in defaults only)")?;
        }
        for source in &merged.sources {
            writeln!(out, "  {}", source.display())?;
        }
        writeln!(out, "  {}", "command line".dimmed())?;

        writeln!(out, "\n{}", "[keep]".bold())?;
        write_keep(out, "  ", merged.keep())?;
        for (ecosystem, keep) in merged.keep_overrides() {
            writeln!(out, "  [keep.{ecosystem}]")?;
            write_keep(out, "    ", keep)?;
        }

        writeln!(out, "\n{}", "exclude".bold())?;
        for pattern in &merged.exclude {
            writeln!(out, "  {pattern}")?;
        }
        if merged.exclude.is_empty() {
            writeln!(out, "  (none)")?;
        }

        writeln!(out, "\n{}", "include".bold())?;
        if merged.include.is_empty() {
            writeln!(out, "  (none)")?;
        }
        for pattern in &merged.include {
            writeln!(out, "  {pattern}  {}", "swept without a marker".dimmed())?;
        }

        writeln!(out, "\n{}", "[[paths]]".bold())?;
        let mut any = false;
        for (pattern, keep, ops) in merged.rules() {
            any = true;
            writeln!(out, "  match = \"{pattern}\"")?;
            write_keep(out, "    ", keep)?;
            for (spec, enabled) in ops {
                writeln!(out, "    {} {spec}", if *enabled { "enable" } else { "disable" })?;
            }
        }
        if !any {
            writeln!(out, "  (none)")?;
        }

        writeln!(out, "\n{}", "artifacts differing from the catalog default".bold())?;
        let mut changed = false;
        for ecosystem in CATALOG {
            for (offset, artifact) in ecosystem.artifacts.iter().enumerate() {
                let spec = format!("{}.{}", ecosystem.id, artifact.slug());
                let Some(id) = crate::classify::ArtifactId::from_spec(&spec) else {
                    continue;
                };
                let _ = offset;
                let enabled = merged.selection().is_enabled(id);
                if enabled != artifact.default_on {
                    changed = true;
                    writeln!(out, "  {} {spec}", if enabled { "on " } else { "off" })?;
                }
            }
        }
        if !changed {
            writeln!(out, "  (none)")?;
        }
        Ok(())
    };

    write(out).map_err(|error| crate::error::Error::Config {
        path: root.to_path_buf(),
        reason: error.to_string(),
    })
}

fn write_keep(out: &mut dyn io::Write, indent: &str, keep: KeepPolicy) -> io::Result<()> {
    if keep.is_empty() {
        return writeln!(out, "{indent}(unset)");
    }
    if let Some(min_age) = keep.min_age {
        writeln!(out, "{indent}min_age  = {min_age:?}")?;
    }
    if let Some(min_size) = keep.min_size {
        writeln!(out, "{indent}min_size = {min_size}")?;
    }
    if let Some(max_size) = keep.max_size {
        writeln!(out, "{indent}max_size = {max_size}")?;
    }
    Ok(())
}
