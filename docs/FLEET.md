# Operating a runner fleet

`inventory/fleet.toml` is the local, Git-ignored source of truth for your self-hosted GitHub
Actions runners. It declares the organization, its trusted runner group, every host and runner.
The app compares it with GitHub and with the native service on each machine, shows the result
on each machine's and runner's pane, and makes changes only after you review them. The format,
rules and guards come from Runner Fleet's `bin/fleet` CLI.

The fleet uses long-lived native services: no Docker, Kubernetes, ARC, Ansible or VMs.
GitHub's runner auto-update stays on.

> Checking (status, plan, runner checks, logs) only reads. **Apply** never retires an existing
> runner. **Retire** needs an online, idle, active replacement and the phrase typed, and keeps
> the old directory. **Decommission** needs `decommission = true` in the inventory first, and
> also keeps the directory. Every change opens a review; when you run it, the host is read
> again and nothing happens if it changed since the review.

## Setup

The control Mac needs the tools in the README's [Requirements](../README.md#requirements).
Create your local inventory from the fictional example if it doesn't exist:

```sh
cp -n inventory/fleet.example.toml inventory/fleet.toml
```

Replace the example organization, SSH targets, service accounts and runners with your own
(see [Inventory format](#inventory-format)). Keep private addresses and host-specific operating
notes in `fleet.toml`; only the example is tracked. Override the path with
`RUNNER_DASHBOARD_INVENTORY`. Tests read the example and use fake GitHub and SSH boundaries, so
they don't need your private inventory.

Changes use the GitHub CLI login, never the token stored in Settings. Listing organization
runners needs the `admin:org` scope:

```sh
gh auth refresh --hostname github.com --scopes admin:org
```

For a narrower long-term credential, give `gh` a fine-grained token
(`gh auth login --with-token`) with organization **Self-hosted runners: Read and write**. The
control Mac is the only machine with a durable GitHub credential; the runner machines never get
`gh` or a copy of it. Registration and removal tokens are minted at the moment of use, sent
only over SSH stdin, and never written to the inventory or to generated config.

Before the first apply, create the organization runner group named in your inventory
(**Acme trusted builders** in the example). Give it all private organization repositories and
all workflows in them, with public repositories disabled. That covers every current and future
private organization repository without allowlists. Apply checks this policy and refuses to
register into a looser group; the app never creates or loosens the group.

These are persistent machines: anyone who can change a workflow in an eligible private
repository can run code on them. Keep the runner group restricted to private repositories and
limit write access.

## Inventory format

```toml
[organization]
name = "acme"
url = "https://github.com/acme"
runner_group = "Acme trusted builders"

[[hosts]]
id = "linux"
platform = "linux-x64"
ssh = "builder@linux.example.ts.net"
ssh_transport = "tailscale"
root = "~/gh-runners/acme"
service_mode = "systemd"
labels = ["fleet", "linux-build", "host-linux"]
runners = [{ name = "linux-01", directory = "runner-01" }]
```

`[organization]` names the organization, its URL (`https://github.com/NAME`) and the runner
group every runner registers into. Each `[[hosts]]` entry is one machine:

| Key | Meaning |
|---|---|
| `id` | Unique host ID. The local API accepts `inv:<id>` for the host's machine. |
| `platform` | `darwin-arm64`, `windows-arm64`, `windows-x64` or `linux-x64`. |
| `ssh` | SSH target, or `${NAME}` to read it from the environment variable `NAME`. |
| `ssh_transport` | `openssh` (default) or `tailscale`. |
| `root` | The dedicated directory that holds this host's runners. |
| `service_mode` | `launchagent` on macOS, `systemd` on Linux, `windows-service` or `windows-logon` on Windows. |
| `service_account` | Windows only, and required for `windows-logon`. |
| `labels` | Labels on every runner of the host: `fleet`, one platform capability label (`macos-build`, `windows-build`, `linux-build` or `linux-experimental`) and, by convention, `host-<id>`. |
| `enabled` | `false` leaves the host out of checks and changes. Defaults to `true`. |
| `runners` | At least one runner. |

A runner takes `name`, unique across the inventory, and `directory`, one path component below
`root`. Optional keys: `labels` (added to the host's), `exclude_labels` (host labels this runner
doesn't get; never `fleet`), `replaces = { name, directory }` for the legacy runner it
supersedes (add `repository = "org/repo"` for a repository-level one), and
`decommission = true`. Validation rejects duplicate names or paths, traversal, the wrong service
mode for the platform, and roots outside the runner area.

## The example fleet

[`inventory/fleet.example.toml`](../inventory/fleet.example.toml) demonstrates macOS LaunchAgents,
Linux systemd, Windows services and Windows logon tasks. It includes a legacy migration, a
decommissioned runner, a GUI lane separated from build work and a Linux canary.

Keep actual hardware, capacity and unattended-setup notes in your private inventory's comments.
Runners live in one dedicated root per host, for example:

```text
macOS/Linux: ~/gh-runners/acme/runner-01
Windows:     C:\gh-runners\acme\runner-01
```

## Using the app

Machines come from the tailnet. Each inventory host is matched to its tailnet machine (as
`ssh -G` resolves its target) and listed with its runners: declared runners, legacy runners
still present, runners pending decommission, and GitHub runners carrying its `host-*` label.

- **Check now** (overview) or **Check** (a machine) reads GitHub with `gh` and probes hosts over
  SSH (`tailscale ssh` for `ssh_transport = "tailscale"`). The app checks once at launch, then
  on demand; health and local runner facts are also read over SSH every minute. An unreachable
  GitHub or host shows as *unknown*, never as missing, and one failure doesn't hide the other
  hosts.
- **Run checks** runs the runner checks (`doctor` in the CLI and the local API): SSH identity,
  prerequisites, disk, power policy and, for logon-task hosts, auto-logon and the active
  session. Results show on each machine.
- **Review apply…** (a machine's Changes section) installs or reconciles that host's runners:
  missing runners, labels, trusted-group membership, stopped services. On a host whose runners
  replace legacy ones, it applies one runner at a time.
- On a runner's pane: **Restart…** (only when it's idle), **Retire…**, **Decommission…**, and
  **Diagnostic log ▸ Show last 200 lines**, a read-only tail of its newest `_diag/Runner_*.log`.
- The Windows service-account password (`service_mode = "windows-service"` with
  `service_account`) and a Linux sudo password (for hosts without non-interactive sudo) are
  typed into the review when needed, used once, and never stored.

A new runner is installed like this:

1. Refuse GitHub-name collisions and non-empty, unexpected directories.
2. Fetch the current Actions runner release metadata from GitHub.
3. Download on the machine and verify the SHA-256 digest GitHub publishes.
4. Mint and send a short-lived registration token.
5. Register directly into the trusted group (`--runnergroup`), configure the directory and
   native service, and hold the service stopped.
6. Check and fix custom labels and group membership through the API.
7. Start the service only then, so an interrupted apply leaves a runner that can't take work.

Labels on an existing runner are changed through the labels API. A runner is never
re-registered and `--replace` is never used.

For a read-only JSON report without the app:
`cargo run -q -p fleet --example fleet -- status --json` (also `inventory`, `plan`, `doctor`,
and `health HOST`).

## Parity with Runner Fleet

`scripts/fleet-parity.sh` runs the read-only commands (`inventory`, `status`, `plan`, `doctor`)
through Runner Fleet's `bin/fleet` and through this port, and compares the JSON. It needs a
Runner Fleet checkout (`RUNNER_FLEET_DIR`, default `~/Code/soloterm/runner-fleet`) and python3.
It reads `RUNNER_DASHBOARD_INVENTORY` like the app. Free-disk numbers and runners outside the
inventory are ignored, since they move between the two runs.

```sh
scripts/fleet-parity.sh              # all four commands
scripts/fleet-parity.sh plan doctor  # selected commands
```

## Workflow routing

Normal jobs route by the trusted group and a capability label, never a host:

```yaml
jobs:
  macos-build:
    runs-on:
      group: Acme trusted builders
      labels: macos-build
```

Use `windows-build` or `linux-build` for the other platforms, and add `xcode` when the job needs
Xcode. `host-*` labels are for diagnostics, not workload placement.

GUI E2E jobs select `macos-e2e`, `windows-e2e` or `windows-e2e-x64`. These are runner labels in
the inventory, never host labels. One eligible runner per lane serializes GUI jobs on that
desktop while builds use the other runners. The GUI slots use
`exclude_labels = ["macos-build"]` so builds can't occupy the desktop. Keep every label in the
inventory: apply removes custom labels the inventory doesn't declare.

Labels only govern GitHub scheduling. Manual GUI runs over SSH must use the desktop repository's
E2E harness and its singleton lock. A healthy runner service doesn't prove an interactive
desktop is ready.

## Native services by platform

| Platform | Service | Operating condition |
|---|---|---|
| macOS | per-user LaunchAgent | Jobs use the service owner's logged-in session (Xcode, keychain, GUI). Runners start after that user logs in following a reboot. |
| Linux | systemd | Starts before login and survives reboot. Install needs non-interactive sudo (or the sudo password in the review) for dependencies and the service. |
| Windows | service (`windows-service`) | Starts without login. Harden AC sleep and reboot policy, and make sure the service identity can reach every tool and certificate it needs. |
| Windows | logon task (`windows-logon`) | Runs `run.cmd` in `service_account`'s interactive desktop session, so GUI/WebView tests see a real desktop instead of Session 0. Needs auto-logon for that account; runner checks verify it. |

**Windows service identity.** The default runner service identity is `NETWORK SERVICE`, which
has no developer profile, certificates or per-user tool state. When jobs need a real build
user, set `service_account` (for example, `.\builder`) to keep its tool and profile state. The
password is asked for in the review. Runner checks can't prove a signing certificate is usable;
check that with a trusted test workflow.

**Interactive Windows runners** (`service_mode = "windows-logon"` with `service_account`).
Apply configures the runner without `--runasservice` and gives the account Modify on the runner
directory. It registers a Scheduled Task, `runner-fleet-NAME`, that starts `run.cmd` at that
account's logon with an interactive, non-elevated principal, under a headless console so no
window can be closed by accident. Task Scheduler only retries launch failures, so a second
trigger repeats every 5 minutes; it's skipped while the runner runs and only fires while the
account is logged on. The task stays disabled until group membership and labels are verified.
A `.fleet-task` marker in the runner directory lets status, restart, retire and decommission
find it; the runner counts as running when its own `Runner.Listener` is alive. No password is
sent. Set up auto-logon with Sysinternals Autologon before applying (it stores the credential
as an LSA secret). Keep that desktop dedicated to CI: signing another user in at the console
disconnects the runner's session, leaving it online without a live display.

**Modern Standby.** On an S0 laptop, turning the display off can enter standby even with sleep
set to never. Check AC sleep, display-off, lid actions and networking connectivity in standby
if the runner, Tailscale or SSH disconnects. A local policy at
`HKLM\SOFTWARE\Policies\Microsoft\Power\PowerSettings\f15576e8-98b7-4186-b944-eafa664402d9`
overrides the plan; its `ACSettingIndex` must be `1`, and a change takes effect after reboot.
Runner checks report each `power:*` setting. Nothing here ever changes power settings.

Runners that share a user account also share its caches and machine access; concurrent jobs
are not isolated. Record each host's capacity and operating constraints in the private inventory.

**Tailscale SSH hosts** use `ssh_transport = "tailscale"` and explicit
`builder@HOST.example.ts.net` targets; scripts and transient passwords go over stdin. Tailscale
SSH must already be enabled and authorized on the machine. On macOS that means the open-source
`tailscaled`; the App Store and standalone apps don't serve Tailscale SSH.

**NixOS.** Before the first apply, copy
[`crates/fleet/scripts/nixos-runner-prerequisites.nix`](../crates/fleet/scripts/nixos-runner-prerequisites.nix)
to `/etc/nixos/runner-fleet-prerequisites.nix`, import it, and run
`sudo nixos-rebuild switch`. It supplies `nix-ld` libraries and `/bin/bash` for the runner
and its auto-update scripts. Apply then generates a per-runner module,
`/etc/nixos/runner-fleet-NAME.nix`, imports it while keeping a backup of
`configuration.nix`, and rebuilds; the service stays closed until labels and group membership
are verified. Don't also enable `services.github-runners` for inventory runners.

## Add a host

1. Make sure it's on the tailnet and reachable over SSH from this Mac: Remote Login on macOS,
   OpenSSH Server on Windows, or Tailscale SSH. Its host key should already be trusted; the app
   never adds host keys.
2. Add it to `inventory/fleet.toml` (see [Inventory format](#inventory-format)):

   ```toml
   [[hosts]]
   id = "ubuntu-mini-01"
   platform = "linux-x64"
   ssh = "builder@ubuntu-mini-01.example.ts.net"
   ssh_transport = "tailscale"
   root = "~/gh-runners/acme"
   service_mode = "systemd"
   labels = ["fleet", "linux-build", "host-ubuntu-mini-01"]
   runners = [{ name = "ubuntu-mini-01-01", directory = "runner-01" }]
   ```

3. In the app: **Check now**, then **Run checks** for the new host, then **Review apply…**.

## Replace and retire a runner

Never move or rename a configured runner directory: LaunchAgent plists hold absolute paths.
Declare the new runner with `replaces = { name = "...", directory = "..." }`, install it beside
the old one, check it, and only then retire the old one. Capacity never drops.

1. **Review apply…** for the new runner only. The old one is untouched.
2. **Check now**: both online, both services active. Let it take a real CI job before
   retiring anything.
3. The legacy runner's pane now offers **Retire…** (the plan says *retire-legacy*, not
   *preserve-legacy*). Type the phrase it asks for. The old directory is kept for recovery.

Stop if GitHub or the host can't be read, the new service is inactive, the replacement is
offline or busy, or the old runner is busy. Runner workspaces are disposable: `_work` isn't
copied.

## Decommission a runner

Remove capacity without a replacement by declaring it first, so no later apply reinstalls it:

```toml
runners = [{ name = "linux-01", directory = "runner-01", decommission = true }]
```

The runner then shows *decommission pending*, and its pane offers **Decommission…**. It refuses
a busy runner. It also refuses, for manual inspection, a GitHub entry whose directory isn't
configured and a configured directory with no GitHub entry. Otherwise it stops and uninstalls
that exact service and deregisters with a short-lived removal token; on NixOS it also removes
the generated module, keeping a `configuration.nix.before-decommission-NAME` backup. An offline
registration with no directory is deleted through the API. It always waits until GitHub stops
listing the name and keeps the runner directory. The entry stays in the inventory as a
tombstone that reserves the name and path; remove it, and the directory, later and
deliberately.

## Recovery

- **Install failed before registration:** fix the reported prerequisite. A partial, unconfigured
  extraction in the new directory needs manual inspection; nothing overwrites unexpected data.
- **Registered, but the group or labels failed:** don't retry blindly. The plan shows the
  drift, and **Review apply…** fixes it through the API.
- **Service down, runner idle:** **Restart…**, then **Check now** and **Diagnostic log ▸ Show
  last 200 lines**.
- **Retire or decommission interrupted:** **Check now** and inspect both registrations. If the
  service is gone but GitHub still lists the runner, decommission again. Never re-register a
  decommissioned name without first removing its tombstone.
- **Restoring a retired runner:** its files are still at the legacy path, but its registration
  is gone. That's a manual procedure, not an automatic rollback.

There's no recursive cleanup. Remove old directories by explicit path, separately.

## Troubleshooting

| Symptom | Check |
|---|---|
| GitHub columns are *unknown* | `gh auth status --hostname github.com`; refresh `admin:org`, or re-run `gh auth login` with a suitable token. |
| "runner group 'NAME' does not exist; create and restrict it in GitHub first" | Create the group named in the inventory and restrict it. The name is case-sensitive. |
| Host can't be read | Check whether it is awake and reachable over Tailscale and SSH. |
| macOS runners offline after a reboot | Log in as the LaunchAgent owner. |
| Linux install fails on sudo | Use the sudo password field in the review, or configure narrowly scoped non-interactive sudo. |
| Windows job can't see a certificate or profile | Check the runner's service identity; use a dedicated build account only when needed. |
| Logon-task runner offline | **Run checks**: auto-logon must be set and the account's session active. |
| GUI E2E fails with focus or visibility errors on a Mac | A locked desktop fails every GUI test: check `IOConsoleUsers`' `CGSSessionScreenIsLocked`. |
| "GitHub name exists but exact local directory is missing" | Resolve the collision by hand. Apply never uses `--replace`. |
| Local `.runner` exists but GitHub has no runner | Inspect before re-registering. Apply won't tear it down. |
