# 0017 — Docker Housekeeping: Opt-In, Never a Container's State, and the Blocks Handed Back

- Status: Accepted
- Date: 2026-10-17

## Context

On the machine this was written on, `docker system df` reported **85 GB of images (65 GB
reclaimable), 158 local volumes, and 6.8 GB of build cache**, with six containers running. Colima
holds all of it in one sparse disk image on the host, and that image was **120 GB on the host while
the VM's filesystems held 80 GB**. Deleting things inside the VM does not shrink the file: the host
gets blocks back only when the guest tells the hypervisor which of them are free, which is what
`fstrim` does, and nothing runs it. A prune that reports "reclaimed 60 GB" can therefore leave the
host's disk exactly as full as before.

Two things make this unlike every other stage in voom:

- It acts through a **daemon**. The Bazel and Claude stages read and delete files voom can see and
  prove things about; here a command is sent to a process that decides, and the state it decides on
  (containers, mounts) is not on any path voom walks.
- Most of what Docker would remove is **not regenerable by a marker**. An image can be pulled again;
  a volume cannot, and a stopped container can hold state a user meant to come back to.

`docker system prune` and `docker volume prune` are the obvious tools, and both are wrong as a
default: the first removes stopped containers, and the second used to remove every unused volume —
named ones included — until daemon API 1.42.

## Decision

**voom prunes Docker only when asked, removes only what no container refers to, treats volumes as
data behind a second and third opt-in, and — when the daemon lives in a VM voom can identify —
trims the guest afterwards and reports the host bytes that came back.**

### Opt-in, from three places

A sweep prunes Docker only under `--docker` (or `[docker] enabled = true` in the user's own
configuration); `voom docker-prune` is the explicit surface and needs no flag. It is off by default
because it talks to a daemon. Like `[bazel]` and `[caches]`, `[docker]` is machine-global and read
only from the user configuration or `--config`; a repository's `voom.toml` that sets it is a hard
error.

### What is removed

| Lane | Removed | Command |
| --- | --- | --- |
| Images | every dangling image; every image no container was made from older than `--docker-max-age` (default `7d`) | `image prune --force`, then `image prune --all --force --filter until=<hours>h` |
| Build cache | records no build is using and nothing has read for `--docker-max-age` | `builder prune --all --force --filter until=<hours>h` |
| Volumes | **only with `--docker-volumes`**: anonymous volumes no container mounts. **Only with `--docker-volumes-all`**: every volume no container mounts | `volume prune --force` / `volume prune --all --force` |

Never run, in any mode: `container prune`, `system prune`, any `rm`. Stopped containers are kept
because they can hold state. An image any container was made from — running or stopped — is kept,
because Docker's own "unused" means exactly that. A volume any container mounts is kept for the same
reason. The age is mapped to `until=` in whole hours, **rounded up**, so the plan never reaches
closer to now than the age that was asked for; `until=` filters images on creation time and build
cache on last use.

The age applies to the build cache too, which departs from "all unused cache". Cache is what makes
the next `docker build` fast, and a record a build read this week is the one that matters; a record
nothing has read for a week is the unused one.

Two guards sit under the volume lane. A volume count or container count the inventory cannot read
(`N/A`) is treated as in use, never as zero. And on a daemon older than API 1.42 the anonymous form
is **skipped**, not run, because there `volume prune` means "every unused volume".

### A dry run is the same plan

One `docker system df -v --format json` inventory — taken in parallel with the `version` probe and
the context lookup — feeds one planner, and the planner produces both the figures a dry run prints
and the exact argument lists the real run executes. The dry run withholds the commands and the
trim; it is not a second estimator (`CONTRIBUTING.md`). Its selection applies Docker's filters to
the inventory: unused is a container count of zero, old is `CreatedAt` (images) or `LastUsedAt`
(cache) before `now - hours`.

The estimate is a **lower bound** for images: it sums each selected image's `UniqueSize`, the layers
no other image shares. Two selected images sharing a layer that no kept image uses free it too, and
the inventory cannot see that. On a classic (non-containerd) image store, an unused image that is a
parent of a used one is kept by Docker but looks selectable here. The real run reports the **bytes
Docker itself printed**, summed over its commands — not the estimate.

### Trim, and the host bytes it returns

After a real prune the backend is placed from the daemon endpoint (`DOCKER_HOST`, else the current
context's host) and, for the backends where a VM holds the disk, a trim runs inside it:

| Endpoint | Backend | Trim |
| --- | --- | --- |
| `…/.colima/<profile>/docker.sock` | Colima | `colima ssh -p <profile> -- sudo fstrim -av` |
| `…/.lima/<instance>/…` | Lima | `limactl shell <instance> sudo fstrim -av` |
| `…/.rd/docker.sock` | Rancher Desktop | `rdctl shell sudo fstrim -av` |
| `…/podman-machine-<name>-api.sock` | podman machine | `podman machine ssh <machine> sudo fstrim -av` |
| `…/.docker/…`, `…/.orbstack/…` | Docker Desktop, OrbStack | none — they trim themselves |
| `/var/run/docker.sock` | native Linux | none — no VM |
| anything else | unknown | none — the command inside it is not something to guess |

The host disk image is measured before and after by `st_blocks` — allocated blocks, not the
apparent length a sparse file never changes — and the difference is reported as host bytes
returned. The trim runs on every real run, even when the prune freed nothing, because the case that
motivated it (40 GB held by blocks freed long ago) is exactly that. `--no-trim` turns it off.
`VOOM_DOCKER_DISK_IMAGES` replaces the guessed image paths, for tests and unusual layouts.

### Never fails the sweep

A missing `docker`, a stopped daemon, a refused socket, or a timeout is a **skip**: one line on
stderr in a sweep, one line (or `"status": "skipped"`) from the subcommand, exit `0`. A prune
command that fails is a failed lane in the report, and also leaves the exit code alone. Every call
is bounded: queries by one minute, each prune command and the trim by `--timeout` (default `15m`).
The independent lanes — images, cache, volumes — run in parallel; the commands inside a lane run in
order. In a sweep the whole stage runs beside the scan on its own thread.

### Output

Human output follows the Bazel stage: silent in a sweep when there is nothing to say. `--format
json` on the subcommand is one document (`schema_version`, `lanes`, `trim`, `totals`); in a sweep
stdout stays the sweep's single document and the Docker summary is one line on stderr, as for Bazel.
Sizes are decimal, matching Docker's own, so a figure can be set beside `docker system df`.

## Consequences

Positive:

- Reclaims the space a developer machine actually loses to Docker, and — the part `docker system
  prune` never does — gets it back to the host on the VM backends.
- The default set cannot lose a container's state, a named volume, or an image a container uses.
- Dry run and real run agree by construction, and the real run's number is Docker's.

Negative / risks:

- **The dry-run image figure is a lower bound**, and on a classic image store may include an image
  Docker would keep for being a parent. The report says "would", and the real run's figure is the
  one to trust.
- **`--docker-volumes-all` deletes data.** A named volume with no container today (a stopped
  compose project's database) is "unused" to Docker and removed. It is a separate, long flag, never
  settable from a file, for that reason.
- **The trim needs `sudo` inside the VM**, which Colima and Lima grant without a password and
  others may not; a refusal is a note, not a failure.
- **A trim on every real run costs seconds to minutes** on a large disk. It is the price of the
  host bytes, and `--no-trim` is the way out.
- **The hour granularity of `until=`** means `--docker-max-age 90m` prunes at two hours.
- Docker's text output (`Total reclaimed space:`, buildx's `Total:`) is parsed because the prune
  commands have no JSON form. An unrecognised total is reported as "no size", never as zero.

## Alternatives considered

- **`docker system prune`**: rejected — removes stopped containers, and a state-holding container is
  the one thing this must not touch.
- **Plain `docker volume prune` for the anonymous form, on every daemon**: rejected — on API < 1.42
  it removes named volumes. Gated on the daemon version instead.
- **Removing the selected volumes by name (`volume rm`)**: considered for precision; rejected for
  the default because `volume prune` reports bytes in Docker's own accounting, and the version gate
  closes the one case it protected against.
- **Estimating a dry run from `docker image ls` alone**: rejected — its `UniqueSize` is `N/A` on
  some stores, and `system df -v` is one call that carries images, volumes and cache together.
- **Trimming only when the prune freed something**: rejected — the measured 40 GB gap was freed
  blocks from earlier, which a conditional trim would never reach.
- **Trimming unknown backends by guessing a shell**: rejected on the rule every stage here follows:
  no proof, no action.
- **Sharing `git/run.rs`'s process runner**: not done. It is private to the git stage and sets
  git-specific environment; a ~40-line copy that cannot disturb it is the smaller coupling.
