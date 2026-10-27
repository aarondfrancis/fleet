# Runner Dashboard architecture

A macOS app with a local Rust core, a React UI, and read-only monitoring over Tailscale and SSH.
Nothing is installed on the machines. A bounded local journal keeps observed events.

## Code layout

| Path | Responsibility |
| --- | --- |
| `crates/fleet` | GitHub and Tailscale reads, SSH collection, desired state, view derivation, demo data |
| `src-tauri` | Desktop commands, menus, windows, polling and GitHub Keychain storage |
| `src` | React screens and generated Rust view bindings |
| `inventory/` | The private `fleet.toml` and the tracked example; see [FLEET.md](FLEET.md) |

## Data flow

Tailscale is read every 15 seconds, GitHub runners every 20 seconds, GitHub jobs every 45 seconds,
and machines over SSH every 60 seconds after the previous round completes. Refresh Now wakes the
pollers. SSH rounds coalesce and use at most four concurrent sessions. Blocking remote commands
run off the async runtime, with a 30-second timeout per machine.

`machine.rs` combines the native health script with the inventory runner probe in one SSH
session. macOS uses sysctl, vm_stat, top and df; Linux uses procfs and df; Windows uses CIM.
An unavailable metric remains null. Truncated responses, invalid numbers and incomplete runner
lists fail the read. The runner probe checks only declared and legacy paths and native service
state. No runner credential files are read.

The same SSH session includes native machine diagnostics. `machine_diagnostics.rs` interprets
selected facts from Tailscale, pmset, systemd/logind or Windows powercfg, alongside disk and runner
facts. Only selected Tailscale preference fields are returned; raw preferences and private state
never leave the machine. Checks distinguish pass, warning, information and unknown. An incomplete
diagnostic section becomes unknown without discarding otherwise valid health and runner data.
Diagnostics inherit the health observation's timestamp and freshness. Fleet warnings use only
fresh, connected observations, and include discovered machines without inventory entries.

The inventory chooses `ssh` or `tailscale ssh`. OpenSSH uses batch mode, trusted host keys, no
agent forwarding and no port forwarding. Scripts travel on stdin, never as long command-line
arguments; Windows scripts are a base64 line read with `[Console]::In.ReadLine()`, so nothing
waits on stdin EOF through cmd.exe. Discovered machines outside the inventory can report health
using their tailnet address and advertised Tailscale SSH, or this Mac's OpenSSH configuration;
runner paths still come from the inventory.

Each result is a timestamped observation. Errors are recorded beside the last successful read.
`derive(config, observations, plan, now, source)` produces the complete view without I/O.
Machines use `ts:<node id>` as their UI identity, or `inv:<host id>` without a tailnet match.
Inventory SSH aliases resolve through `ssh -G`. Other desktop and server machines appear under
Other machines; ignored machines and phones don't get automatic SSH reads.

The UI receives fleet-updated events after polls and every five seconds so ages advance.

## Event history and alerts

`monitoring/` evaluates transitions every five seconds and writes `monitoring.json` beside the
app settings, off the async runtime. It keeps at most 30 days and 5,000 events, and checkpoints
baselines at least once a minute. History is collected only while the app runs; it can't
reconstruct events between checks. Transitions are saved atomically before a notification can
be requested; failed writes keep notifications pending, and corrupt or newer-format files are
preserved with a visible storage error. History pages use an exclusive, monotonic event-ID
cursor.

Offline alerts require three continuous minutes of fresh tailnet observations, with a healthy
local Tailscale connection; a local Tailscale outage, stale data or a gap in observations
doesn't accrue time. Low-disk alerts require distinct SSH samples below 10% free at least 60
seconds apart, and clear at 12%. Active disk conditions are grouped into one alert per machine;
equal capacity readings share one detail line. Unknown or stale observations can't clear an
alert. OS boot-session IDs detect observed reboots; uptime jitter doesn't. Service events
describe observed state transitions and can't count restarts between samples.

Desktop notifications are opt-in and batched per monitoring round by the Tauri shell. Each new
alert and recovery is announced once, after its history is saved. A one-hour maintenance pause
suppresses delivery for that machine while keeping events and in-app alerts; an unresolved
alert first raised during the pause is announced once when it ends. Persisted baselines and
announcement state prevent duplicates after a restart. Stable inventory keys preserve history
across tailnet node replacements; a changed SSH target resets alert and boot baselines. Demo
history is fictional, generated separately, and never touches the journal or sends alerts.

## Status rules

- Installed/configured and service state come from SSH; connected, busy and idle come from GitHub.
  A running service doesn't prove that a runner is idle.
- Machine facts become stale after 150 seconds or a failed check. Old values remain visible with
  their original timestamp. A changed SSH target never inherits the previous target's health.
- GitHub runners become stale after 150 seconds; jobs after 300 seconds. Without fresh GitHub
  runner data, activity is unknown. Current jobs require an actual runner_id match in the jobs API.
- Elapsed time never labels a job stuck. Machines and runners sort by name, independent of status.
- Queued jobs list label-compatible runners, not a promise of scheduling; group access can also
  affect eligibility.
- Fresh SSH runner facts update displayed inventory drift. Every change still re-reads the host
  through the desired-state engine before review and again before execution.

## GitHub and local access

Monitoring uses the GitHub CLI login by default, held in memory, or a pasted token kept in the
macOS Keychain. Changes always use the `gh` login ([FLEET.md](FLEET.md#setup)). Org runner lists
and per-repository workflow jobs are fetched separately; repository scope is configurable in
Settings.

An account can also list runner repositories, whose repository-level runners
(`/repos/{owner}/{repo}/actions/runners`) are polled with the org list and shown beside its
runners. A personal account has only these, and scans its runner repositories for jobs. They're
monitoring only: never joined to inventory hosts, and only compatible with their own
repository's queued jobs. The org list and each repository keep their own read time and error,
so one that can't be listed keeps its last runners (aging into stale) and hides no others; only
a rejected token or a rate limit fails the account. Adding an account needs every list to
succeed; changing its sign-in only needs the org list, or one repository for a personal account.

Recent `actions/runner` releases (no drafts or pre-releases) are read at most hourly through the
first github.com account that can; releases read over a day ago aren't compared. A runner whose
GitHub-reported version is behind the latest has an update available, shown on its pane. It's
overdue, and a finding, when the oldest release it's missing was published over 30 days ago,
since GitHub then stops sending it jobs. Nothing upgrades runners.

The local HTTP API ([HTTP_API.md](HTTP_API.md)) captures one fleet snapshot per request off
the async runtime and adds provenance and a content-based revision to the response. The shared
`issues` derivation feeds both the desktop's attention rows and `/v1/issues`; active alerts
replace duplicate disk and offline rows. The OpenAPI 3.1 contract derives view schemas from the
Rust types. `POST /v1/checks` runs the same read-only refresh, inventory check and runner checks
as the app, waiting for automatic checks already in flight; the regular poller still coalesces
redundant refreshes.

Demo data passes through the same derivation. Demo mode disables live polling and runner changes.
Tests replace GitHub, SSH and service boundaries; they never contact real hosts or the org.

## Desired state (Runner Fleet, merged)

`crates/fleet/src/desired/` ports Runner Fleet's Python (`model`, `inventory`, `github`,
`remote`, `state`, `plan`, `operations`), with its tests in `desired/tests.rs`. It keeps the
file format, rules and messages, including `exclude_labels` and the `windows-logon` service mode
(`desired/windows_logon.rs`), where the runner runs in an auto-logged-on desktop session for GUI
tests. It differs only in the hardening listed below. The machine scripts
(`crates/fleet/scripts/`) are embedded with `include_str!`, and their RF_* values travel in the
script on SSH stdin.

- **Boundaries:** `CommandRunner` runs `gh`, `ssh` and `tailscale`. `FleetGithub` and
  `FleetRemote` are traits, so tests fake GitHub and hosts and never contact either.
- **Pure core:** `state::build_status_rows` and `plan::generate_plan` are pure functions of
  (inventory, snapshot). An unreachable GitHub or host becomes "unknown"/`inspect`, never
  "missing".
- **Guards kept from Runner Fleet:**
  - No `--replace`; configured runner directories are never moved or deleted.
  - A name collision or orphan refuses apply.
  - A busy runner is never restarted or removed.
  - Retire needs an online, idle, labelled, in-group, active replacement.
  - Decommission needs `decommission = true`.
  - A migrating host applies one runner at a time.
  - The runner group policy is validated before registering.
- **Hardening:** OpenSSH runs with `StrictHostKeyChecking=yes`, `ForwardAgent=no` and
  `ClearAllForwardings=yes`. Passwords come from the review form, not environment variables,
  held in a `Transient` whose `Debug` hides the value.
- **Engine (`engine_desired.rs`):**
  - `desired_refresh`/`desired_doctor`/`desired_logs` are read-only.
  - `desired_review` re-reads one host and returns a `ReviewView`.
  - `desired_execute` re-reads it again, and refuses unless the regenerated plan items equal
    the reviewed ones. Reviews are single-use and expire after 15 minutes.
  - SSH aliases are resolved off the async runtime so inventory hosts join their tailnet
    machines.
- **Parity:** `examples/fleet.rs` prints Runner Fleet's `--json` shapes for
  [the parity script](FLEET.md#parity-with-runner-fleet).
- **Demo:** `demo_desired.rs` is a fictional inventory and snapshot run through the same view
  builder.
