//! Docker housekeeping — dangling and stale images, idle build cache, and optionally volumes —
//! followed by handing the freed blocks back to the host when the daemon lives in a VM.
//!
//! `docker` is a daemon on the other end of a socket, which is why none of this is on by default:
//! a sweep that talks to it runs a command with side effects on state voom did not scan. The
//! design is in `adrs/0017-docker-housekeeping.md`; the rules that matter here are:
//!
//! - **Nothing a container touches.** Stopped containers are never removed, because they can hold
//!   state; an image any container was made from, running or not, is kept; a volume any container
//!   mounts is kept. Volumes are data and are off unless asked for twice over.
//! - **A dry run is the same plan.** [`prune`] builds the lane plans from one `docker system df -v`
//!   inventory whether or not it will run them, and the dry run withholds only the commands.
//! - **It never fails the sweep.** A missing binary, a stopped daemon or a refused socket is a
//!   [`Skip`] with a one-line reason; a prune that errors is a failed lane in the report.
//!
//! Like [`crate::bazel`] this is machine-global and shares nothing with the sweep, so it runs
//! beside it and reports after.

mod backend;
mod plan;
mod report;
mod run;
mod usage;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use backend::{Backend, DISK_IMAGES_ENV, Trim, TrimOutcome, allocated_bytes};
pub use plan::{LaneKind, LanePlan, VolumeScope};
pub use report::{document, render_human, render_json};

use run::Failure;
use usage::{DiskUsage, parse_reclaimed, parse_usage};

/// The JSON shape's version, versioned separately from the sweep's because it is a separate
/// document sharing only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

/// How old an unused image or an idle cache record must be before it is pruned.
///
/// A week keeps what a working week reuses and drops what only a month-old branch would.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long one prune command or the VM trim may run. Pruning tens of gigabytes of layers, or
/// trimming a large sparse disk, is minutes; an hour would be a hung daemon.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// How long a read-only query (`version`, `system df`, `context inspect`) may take. `system df -v`
/// measures every volume and took 1.4 s over 158 of them; a minute is a daemon that is not
/// answering.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// The program voom delegates to, resolved through `PATH` so a user's own docker wins.
const DOCKER: &str = "docker";

/// What to prune, and under what conditions.
#[derive(Debug, Clone)]
pub struct DockerPruneOptions {
    /// Withhold every prune command and the trim, and report what they would do.
    pub dry_run: bool,
    /// How old an unused image or idle cache record must be.
    pub max_age: Duration,
    /// Which volumes to remove, if any. `None` leaves volumes alone entirely.
    pub volumes: Option<VolumeScope>,
    /// Whether to trim the VM's disk after a prune, when the backend has one.
    pub trim: bool,
    /// The deadline for each prune command and for the trim.
    pub timeout: Duration,
}

impl Default for DockerPruneOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            max_age: DEFAULT_MAX_AGE,
            volumes: None,
            trim: true,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// Why nothing was done. Always a reason to say and move on, never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// One line, in the user's terms.
    pub reason: String,
}

/// What happened in one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneOutcome {
    /// A dry run: the commands were withheld.
    WouldReclaim,
    /// The commands ran. `bytes` is the sum of what docker printed, `None` if it printed none.
    Reclaimed {
        /// Bytes docker reported freeing.
        bytes: Option<u64>,
    },
    /// The lane was not run, with the reason.
    Skipped {
        /// Why.
        reason: String,
    },
    /// A command failed or timed out.
    Failed {
        /// Why, in one line.
        message: String,
    },
}

/// One lane's plan beside what came of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    /// What was planned.
    pub plan: LanePlan,
    /// What happened.
    pub outcome: LaneOutcome,
}

impl Lane {
    /// The bytes this lane accounts for: docker's own figure after a real run, the inventory's
    /// estimate for a dry run, and nothing for a lane that did not run.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        match &self.outcome {
            LaneOutcome::WouldReclaim => self.plan.estimated_bytes,
            LaneOutcome::Reclaimed { bytes } => bytes.unwrap_or(0),
            LaneOutcome::Skipped { .. } | LaneOutcome::Failed { .. } => 0,
        }
    }
}

/// Everything one round produced.
#[derive(Debug)]
pub struct DockerPruneResult {
    /// Whether the commands were withheld.
    pub dry_run: bool,
    /// The age limit in force.
    pub max_age: Duration,
    /// The volume scope asked for, if any.
    pub volumes: Option<VolumeScope>,
    /// Why nothing was done, when nothing was.
    pub skipped: Option<Skip>,
    /// The daemon's endpoint, when it could be read.
    pub endpoint: Option<String>,
    /// The backend, as a report names it, when it could be placed.
    pub backend: Option<String>,
    /// One entry per lane, in a fixed order.
    pub lanes: Vec<Lane>,
    /// The VM trim, when the backend has one and it was not turned off.
    pub trim: Option<Trim>,
    /// Wall-clock time.
    pub elapsed: Duration,
}

/// The footer's numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Images, cache records and volumes matched, across lanes that ran or would.
    pub candidates: usize,
    /// Bytes reclaimed inside the VM, or that would be.
    pub bytes: u64,
    /// Lanes whose commands failed.
    pub failed: usize,
    /// Host bytes the trim handed back, when it was measured.
    pub host_bytes_returned: Option<u64>,
}

impl DockerPruneResult {
    /// The footer's numbers.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();
        for lane in &self.lanes {
            match lane.outcome {
                LaneOutcome::WouldReclaim | LaneOutcome::Reclaimed { .. } => {
                    totals.candidates += lane.plan.candidates;
                    totals.bytes += lane.bytes();
                }
                LaneOutcome::Failed { .. } => totals.failed += 1,
                LaneOutcome::Skipped { .. } => {}
            }
        }
        totals.host_bytes_returned = self.trim.as_ref().and_then(|trim| trim.outcome.returned());
        totals
    }

    /// Whether there is anything worth printing after a sweep: something matched, a prune
    /// failed, or the VM was trimmed. A sweep that found docker clean stays quiet, as it does
    /// for Bazel.
    #[must_use]
    pub fn has_news(&self) -> bool {
        let totals = self.totals();
        totals.candidates > 0
            || totals.failed > 0
            || self
                .trim
                .as_ref()
                .is_some_and(|trim| !matches!(trim.outcome, TrimOutcome::Withheld { .. }))
    }
}

/// Inventories the daemon, then prunes what [`DockerPruneOptions`] allows, then trims the VM.
///
/// Infallible by design: whatever goes wrong is in the result.
#[must_use]
pub fn prune(options: &DockerPruneOptions) -> DockerPruneResult {
    let started = Instant::now();
    let mut result = DockerPruneResult {
        dry_run: options.dry_run,
        max_age: options.max_age,
        volumes: options.volumes,
        skipped: None,
        endpoint: None,
        backend: None,
        lanes: Vec::new(),
        trim: None,
        elapsed: Duration::ZERO,
    };

    // The three queries are independent and the daemon answers them concurrently; the version
    // call doubles as the reachability probe, and fails fast where `system df` would not.
    let (version, inventory, endpoint) = std::thread::scope(|scope| {
        let version = scope.spawn(|| docker(&["version", "--format", "{{.Server.APIVersion}}"], QUERY_TIMEOUT));
        let inventory = scope.spawn(|| docker(&["system", "df", "--verbose", "--format", "json"], QUERY_TIMEOUT));
        let endpoint = scope.spawn(endpoint);
        (joined(version), joined(inventory), endpoint.join().ok().flatten())
    });
    result.endpoint.clone_from(&endpoint);

    let usage = version.and_then(|api| read_inventory(&inventory?).map(|usage| (api, usage)));
    let (api_version, usage) = match usage {
        Ok(found) => found,
        Err(failure) => {
            result.skipped = Some(Skip {
                reason: failure.to_string(),
            });
            result.elapsed = started.elapsed();
            return result;
        }
    };

    let backend = backend::detect(endpoint.as_deref().unwrap_or_default());
    result.backend = Some(backend.name());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0);
    let mut plans = vec![
        plan::images(&usage, now, options.max_age),
        plan::build_cache(&usage, now, options.max_age),
    ];
    if let Some(scope) = options.volumes {
        plans.push(plan::volumes(&usage, scope, api_version.trim()));
    }

    let trim_plan = options
        .trim
        .then(|| dirs::home_dir().and_then(|home| backend::trim_plan(&backend, &home)))
        .flatten();
    result.lanes = if options.dry_run {
        plans.into_iter().map(withhold).collect()
    } else {
        run_lanes(plans, options.timeout)
    };
    result.trim = trim_plan.map(|plan| backend::trim(&backend, &plan, options.dry_run, options.timeout));
    result.elapsed = started.elapsed();
    result
}

fn docker(args: &[&str], timeout: Duration) -> Result<String, Failure> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    run::run(DOCKER, &args, timeout)
}

fn joined<T>(handle: std::thread::ScopedJoinHandle<'_, Result<T, Failure>>) -> Result<T, Failure> {
    handle.join().unwrap_or_else(|_| {
        Err(Failure::Failed {
            message: "a docker query panicked".to_owned(),
        })
    })
}

fn read_inventory(json: &str) -> Result<DiskUsage, Failure> {
    parse_usage(json).map_err(|error| Failure::Failed {
        message: format!("unrecognised `docker system df` output: {error}"),
    })
}

/// The endpoint the client will talk to: `DOCKER_HOST` if set, otherwise the current context's.
fn endpoint() -> Option<String> {
    if let Some(host) = std::env::var("DOCKER_HOST").ok().filter(|host| !host.trim().is_empty()) {
        return Some(host);
    }
    docker(
        &["context", "inspect", "--format", "{{.Endpoints.docker.Host}}"],
        QUERY_TIMEOUT,
    )
    .ok()
    .map(|host| host.trim().to_owned())
    .filter(|host| !host.is_empty())
}

fn withhold(plan: LanePlan) -> Lane {
    let outcome = match &plan.skipped {
        Some(reason) => LaneOutcome::Skipped { reason: reason.clone() },
        None => LaneOutcome::WouldReclaim,
    };
    Lane { plan, outcome }
}

/// Runs the lanes side by side: they touch disjoint things, and docker handles concurrent
/// requests. Within a lane the commands stay in order.
fn run_lanes(plans: Vec<LanePlan>, timeout: Duration) -> Vec<Lane> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = plans
            .into_iter()
            .map(|plan| {
                scope.spawn(move || {
                    let outcome = execute(&plan, timeout);
                    Lane { plan, outcome }
                })
            })
            .collect();
        handles.into_iter().filter_map(|handle| handle.join().ok()).collect()
    })
}

fn execute(plan: &LanePlan, timeout: Duration) -> LaneOutcome {
    if let Some(reason) = &plan.skipped {
        return LaneOutcome::Skipped { reason: reason.clone() };
    }
    let mut reclaimed = Vec::new();
    for command in &plan.commands {
        match run::run(DOCKER, command, timeout) {
            Ok(output) => reclaimed.push(parse_reclaimed(&output)),
            Err(failure) => {
                return LaneOutcome::Failed {
                    message: failure.to_string(),
                };
            }
        }
    }
    // One command's total missing makes the sum a guess, so the lane reports none rather than a
    // figure that is too small.
    let bytes = reclaimed.into_iter().sum::<Option<u64>>();
    LaneOutcome::Reclaimed { bytes }
}
