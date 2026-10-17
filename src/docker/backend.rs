//! Which VM the daemon lives in, and handing its freed blocks back to the host.
//!
//! Docker inside a Colima, Lima, Rancher Desktop or podman-machine VM frees space inside the
//! guest's filesystem only. The host's disk image is a sparse file that does not shrink until the
//! guest tells the hypervisor which blocks are unused, which is what `fstrim` does — so a prune
//! alone can leave the host exactly as full as before. A measured case: a 120 GB image holding
//! 80 GB of files, 40 GB of it recoverable only by trimming.
//!
//! Docker Desktop and `OrbStack` trim for themselves, and a daemon on a Linux host has no VM, so
//! those are recognised and left alone. A backend this file cannot place is left alone too: the
//! command to run inside it is not something to guess at.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use super::run::{self, Failure};

/// Replaces the guessed host disk image paths, a platform path list (`:` on Unix). The measurement
/// is the only thing it changes; which command trims is still decided by the backend.
pub const DISK_IMAGES_ENV: &str = "VOOM_DOCKER_DISK_IMAGES";

/// What a guest needs to give its unused blocks back, verbose so the log names the filesystems.
const FSTRIM: [&str; 3] = ["sudo", "fstrim", "-av"];

/// Where the daemon runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// A Colima profile.
    Colima {
        /// Colima's home, the directory holding `_lima` and the profile directories.
        home: PathBuf,
        /// The profile name, `default` unless `-p` was given.
        profile: String,
    },
    /// A plain Lima instance.
    Lima {
        /// Lima's home, the directory holding the instance directories.
        home: PathBuf,
        /// The instance name.
        instance: String,
    },
    /// Rancher Desktop's VM.
    RancherDesktop,
    /// A `podman machine`.
    PodmanMachine {
        /// The machine name, `podman-machine-default` unless another was made.
        name: String,
    },
    /// A desktop product that reclaims host space by itself.
    SelfTrimming(&'static str),
    /// A daemon on this host's own kernel, so there is no guest disk.
    Native,
    /// Anything else: a remote host, a socket this does not recognise.
    Unknown,
}

impl Backend {
    /// The name a report gives it.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Colima { profile, .. } => format!("colima (profile {profile})"),
            Self::Lima { instance, .. } => format!("lima (instance {instance})"),
            Self::RancherDesktop => "rancher desktop".to_owned(),
            Self::PodmanMachine { name } => format!("podman machine ({name})"),
            Self::SelfTrimming(name) => (*name).to_owned(),
            Self::Native => "native".to_owned(),
            Self::Unknown => "unknown".to_owned(),
        }
    }
}

/// Places a daemon endpoint such as `unix:///Users/me/.colima/default/docker.sock`.
#[must_use]
pub fn detect(endpoint: &str) -> Backend {
    let Some(socket) = endpoint.trim().strip_prefix("unix://") else {
        return Backend::Unknown;
    };
    let path = Path::new(socket);
    let parts: Vec<String> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let file = parts.last().map_or("", String::as_str);
    let home_through = |index: usize| -> PathBuf {
        let mut home = PathBuf::from("/");
        home.extend(&parts[..=index]);
        home
    };

    // Colima keeps `<home>/<profile>/docker.sock`, and the default profile's is also linked at
    // `<home>/docker.sock`. Its home is `~/.colima`, or `colima` under the XDG config directory.
    if let Some(index) = parts.iter().position(|part| part == ".colima" || part == "colima") {
        match &parts[index + 1..] {
            [socket_name] if socket_name == "docker.sock" => {
                return Backend::Colima {
                    home: home_through(index),
                    profile: "default".to_owned(),
                };
            }
            [profile, socket_name] if socket_name == "docker.sock" => {
                return Backend::Colima {
                    home: home_through(index),
                    profile: profile.clone(),
                };
            }
            _ => {}
        }
    }
    if let Some(index) = parts.iter().position(|part| part == ".lima")
        && let Some(instance) = parts.get(index + 1)
    {
        return Backend::Lima {
            home: home_through(index),
            instance: instance.clone(),
        };
    }
    if parts.iter().any(|part| part == ".rd") {
        return Backend::RancherDesktop;
    }
    if file.starts_with("podman-machine") {
        let name = file
            .strip_suffix("-api.sock")
            .or_else(|| file.strip_suffix(".sock"))
            .unwrap_or(file);
        return Backend::PodmanMachine { name: name.to_owned() };
    }
    if parts.iter().any(|part| part == ".orbstack") {
        return Backend::SelfTrimming("orbstack");
    }
    if parts.iter().any(|part| part == ".docker") {
        return Backend::SelfTrimming("docker desktop");
    }
    if matches!(socket, "/var/run/docker.sock" | "/run/docker.sock") {
        return Backend::Native;
    }
    Backend::Unknown
}

/// The command that trims a backend's guest, and the host files whose size it should shrink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrimPlan {
    /// The program, then its arguments.
    pub command: Vec<String>,
    /// Host disk images to measure before and after.
    pub disk_images: Vec<PathBuf>,
}

/// What to run for `backend`, or `None` when nothing needs running or it is not known how.
///
/// `home` is the user's home directory, for the backends whose disk lives under it.
#[must_use]
pub fn trim_plan(backend: &Backend, home: &Path) -> Option<TrimPlan> {
    let words =
        |parts: &[&str]| -> Vec<String> { parts.iter().chain(&FSTRIM).map(|part| (*part).to_owned()).collect() };
    let (command, mut disk_images) = match backend {
        Backend::Colima { home, profile } => (
            words(&["colima", "ssh", "-p", profile, "--"]),
            colima_disks(home, profile),
        ),
        Backend::Lima { home, instance } => (
            words(&["limactl", "shell", instance]),
            ["diffdisk", "disk"]
                .iter()
                .map(|name| home.join(instance).join(name))
                .collect(),
        ),
        Backend::RancherDesktop => (words(&["rdctl", "shell"]), rancher_disks(home)),
        Backend::PodmanMachine { name } => (words(&["podman", "machine", "ssh", name]), podman_disks(home, name)),
        Backend::SelfTrimming(_) | Backend::Native | Backend::Unknown => return None,
    };
    if let Some(overridden) = std::env::var_os(DISK_IMAGES_ENV) {
        disk_images = std::env::split_paths(&overridden)
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
    }
    Some(TrimPlan { command, disk_images })
}

/// Colima names its Lima instance `colima`, or `colima-<profile>`, and its data disk likewise
/// (with an underscore in some releases).
fn colima_disks(home: &Path, profile: &str) -> Vec<PathBuf> {
    let names = if profile == "default" {
        vec!["colima".to_owned()]
    } else {
        vec![format!("colima-{profile}"), format!("colima_{profile}")]
    };
    names
        .iter()
        .flat_map(|name| {
            [
                home.join("_lima/_disks").join(name).join("datadisk"),
                home.join("_lima").join(name).join("diffdisk"),
                home.join("_lima").join(name).join("disk"),
            ]
        })
        .collect()
}

fn rancher_disks(home: &Path) -> Vec<PathBuf> {
    [
        "Library/Application Support/rancher-desktop/lima/0",
        ".local/share/rancher-desktop/lima/0",
    ]
    .iter()
    .map(|dir| home.join(dir).join("diffdisk"))
    .collect()
}

fn podman_disks(home: &Path, name: &str) -> Vec<PathBuf> {
    let providers = home.join(".local/share/containers/podman/machine");
    let Ok(entries) = std::fs::read_dir(providers) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|provider| std::fs::read_dir(provider.path()).ok())
        .flat_map(|files| files.filter_map(Result::ok))
        .map(|file| file.path())
        .filter(|path| {
            let file = path
                .file_name()
                .map(|file| file.to_string_lossy().into_owned())
                .unwrap_or_default();
            file.starts_with(name)
                && Path::new(&file)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("raw") || ext.eq_ignore_ascii_case("qcow2"))
        })
        .collect()
}

/// Bytes the host actually has allocated for these files, summed: `st_blocks`, not the apparent
/// length, because a sparse disk image's length never changes. `None` when none exist or the
/// platform has no such measure.
#[must_use]
pub fn allocated_bytes(paths: &[PathBuf]) -> Option<u64> {
    let sizes: Vec<u64> = paths.iter().filter_map(|path| allocated(path)).collect();
    (!sizes.is_empty()).then(|| sizes.iter().sum())
}

#[cfg(unix)]
fn allocated(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // `st_blocks` counts 512-byte units on every unix, whatever the filesystem's block size.
    std::fs::metadata(path).ok().map(|metadata| metadata.blocks() * 512)
}

#[cfg(not(unix))]
fn allocated(_path: &Path) -> Option<u64> {
    None
}

/// What happened, or would happen, to the guest's free blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trim {
    /// The backend, as [`Backend::name`] gives it.
    pub backend: String,
    /// The command, as it was or would be run.
    pub command: Vec<String>,
    /// What came of it.
    pub outcome: TrimOutcome,
}

/// The result of a trim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrimOutcome {
    /// A dry run: the command was withheld, and the host image holds this much now.
    Withheld {
        /// Host bytes allocated now, when the image could be found.
        host_bytes: Option<u64>,
    },
    /// The guest was trimmed.
    Trimmed {
        /// Host bytes allocated before.
        before: Option<u64>,
        /// Host bytes allocated after.
        after: Option<u64>,
    },
    /// The guest could not be trimmed.
    Failed {
        /// Why, in one line.
        message: String,
    },
}

impl TrimOutcome {
    /// Host bytes handed back, when both measurements exist. Never negative: a disk image that
    /// grew during the trim returned nothing.
    #[must_use]
    pub fn returned(&self) -> Option<u64> {
        match self {
            Self::Trimmed {
                before: Some(before),
                after: Some(after),
            } => Some(before.saturating_sub(*after)),
            _ => None,
        }
    }
}

/// Trims a guest, measuring the host image around it. Under `dry_run` only measures.
///
/// This is also the single place a failure becomes a message: a missing `colima` binary, a VM that
/// is stopped, a `sudo` that wants a password all end as a one-line note and never an error.
#[must_use]
pub fn trim(backend: &Backend, plan: &TrimPlan, dry_run: bool, timeout: Duration) -> Trim {
    let before = allocated_bytes(&plan.disk_images);
    let outcome = if dry_run {
        TrimOutcome::Withheld { host_bytes: before }
    } else {
        let (program, args) = plan
            .command
            .split_first()
            .map_or(("", &[][..]), |(first, rest)| (first.as_str(), rest));
        match run::run(program, args, timeout) {
            Ok(_) => TrimOutcome::Trimmed {
                before,
                after: allocated_bytes(&plan.disk_images),
            },
            Err(Failure::NotInstalled { program }) => TrimOutcome::Failed {
                message: format!("`{program}` is not on PATH, so the VM could not be trimmed"),
            },
            Err(failure) => TrimOutcome::Failed {
                message: failure.to_string(),
            },
        }
    };
    Trim {
        backend: backend.name(),
        command: plan.command.clone(),
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_place_the_default_colima_profile_by_its_socket() {
        assert_eq!(
            detect("unix:///Users/dev/.colima/default/docker.sock"),
            Backend::Colima {
                home: PathBuf::from("/Users/dev/.colima"),
                profile: "default".to_owned()
            }
        );
        assert_eq!(
            detect("unix:///Users/dev/.colima/docker.sock"),
            Backend::Colima {
                home: PathBuf::from("/Users/dev/.colima"),
                profile: "default".to_owned()
            }
        );
    }

    #[test]
    fn should_place_a_named_colima_profile() {
        assert_eq!(
            detect("unix:///home/dev/.config/colima/work/docker.sock"),
            Backend::Colima {
                home: PathBuf::from("/home/dev/.config/colima"),
                profile: "work".to_owned()
            }
        );
    }

    #[test]
    fn should_place_lima_rancher_and_podman() {
        assert_eq!(
            detect("unix:///Users/dev/.lima/docker/sock/docker.sock"),
            Backend::Lima {
                home: PathBuf::from("/Users/dev/.lima"),
                instance: "docker".to_owned()
            }
        );
        assert_eq!(detect("unix:///Users/dev/.rd/docker.sock"), Backend::RancherDesktop);
        assert_eq!(
            detect("unix:///var/folders/xx/T/podman/podman-machine-default-api.sock"),
            Backend::PodmanMachine {
                name: "podman-machine-default".to_owned()
            }
        );
    }

    #[test]
    fn should_leave_self_trimming_native_and_unknown_backends_alone() {
        assert_eq!(
            detect("unix:///Users/dev/.docker/run/docker.sock"),
            Backend::SelfTrimming("docker desktop")
        );
        assert_eq!(
            detect("unix:///Users/dev/.orbstack/run/docker.sock"),
            Backend::SelfTrimming("orbstack")
        );
        assert_eq!(detect("unix:///var/run/docker.sock"), Backend::Native);
        assert_eq!(detect("tcp://10.0.0.5:2376"), Backend::Unknown);
        assert_eq!(detect("unix:///somewhere/else.sock"), Backend::Unknown);
        assert_eq!(detect(""), Backend::Unknown);
    }

    #[test]
    fn should_build_the_in_vm_trim_command_per_backend() {
        let home = Path::new("/Users/dev");
        let command = |backend: &Backend| trim_plan(backend, home).map(|plan| plan.command.join(" "));
        assert_eq!(
            command(&detect("unix:///Users/dev/.colima/work/docker.sock")).as_deref(),
            Some("colima ssh -p work -- sudo fstrim -av")
        );
        assert_eq!(
            command(&detect("unix:///Users/dev/.lima/box/sock/docker.sock")).as_deref(),
            Some("limactl shell box sudo fstrim -av")
        );
        assert_eq!(
            command(&Backend::RancherDesktop).as_deref(),
            Some("rdctl shell sudo fstrim -av")
        );
        assert_eq!(
            command(&Backend::PodmanMachine { name: "m".to_owned() }).as_deref(),
            Some("podman machine ssh m sudo fstrim -av")
        );
    }

    #[test]
    fn should_not_trim_a_backend_that_needs_none_or_is_unknown() {
        let home = Path::new("/Users/dev");
        for backend in [Backend::Native, Backend::Unknown, Backend::SelfTrimming("orbstack")] {
            assert_eq!(trim_plan(&backend, home), None, "{backend:?}");
        }
    }

    #[test]
    fn should_look_for_the_colima_data_disk_under_its_home() {
        let plan = trim_plan(
            &Backend::Colima {
                home: PathBuf::from("/c"),
                profile: "default".to_owned(),
            },
            Path::new("/Users/dev"),
        )
        .expect("colima is trimmable");
        assert!(
            plan.disk_images
                .contains(&PathBuf::from("/c/_lima/_disks/colima/datadisk"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_measure_allocated_blocks_not_apparent_length() {
        let dir = tempfile::TempDir::new().expect("a temp dir");
        let image = dir.path().join("disk");
        let file = std::fs::File::create(&image).expect("creates the file");
        file.set_len(512 * 1024 * 1024).expect("makes it sparse");
        drop(file);
        let allocated = allocated_bytes(&[image]).expect("the file exists");
        assert!(
            allocated < 64 * 1024 * 1024,
            "a sparse 512 MiB file allocates {allocated}"
        );
    }

    #[test]
    fn should_report_nothing_for_images_that_do_not_exist() {
        assert_eq!(allocated_bytes(&[PathBuf::from("/nonexistent/disk")]), None);
    }

    #[test]
    fn should_never_report_negative_bytes_returned() {
        let grew = TrimOutcome::Trimmed {
            before: Some(10),
            after: Some(20),
        };
        assert_eq!(grew.returned(), Some(0));
        let unmeasured = TrimOutcome::Trimmed {
            before: None,
            after: Some(1),
        };
        assert_eq!(unmeasured.returned(), None);
    }
}
