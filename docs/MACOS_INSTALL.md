# macOS package

This page covers the macOS package, which includes the terminal and browser consoles and
its own Node.js runtime. Installed use requires Git; Rust, npm and a source
checkout are unnecessary. For source installation, see [Getting started](GETTING_STARTED.md).


The packaging builder creates a relocatable, architecture-specific compressed tar archive for Apple Silicon (`arm64`) or Intel (`x86_64`). The package keeps `heph`, `hephaestus`, the daemon, reference worker, evaluator, and process guardian together under `bin/`; the daemon and CLI resolve their helper binaries beside themselves. It also contains a bundled Ink TUI, browser console with evidence report export, and a pinned Node.js 24.21.0 runtime. Running the installed consoles does not require a separate Node or npm installation.

## Build an archive

Build on macOS with Rust 1.88 or newer, Git, Python 3, Node.js 22+/npm for console compilation, and the selected Rust target installed. The builder downloads the matching Node.js archive from the official Node distribution and verifies its pinned SHA-256 before including it. Those tools are build prerequisites; only Node is bundled for installed use.

```sh
python3 scripts/package_macos.py --arch arm64
# or, with the x86_64-apple-darwin Rust target installed:
python3 scripts/package_macos.py --arch x86_64
```

The Rust build uses a separate package-build target directory by default. The output archive is written under the target packages directory with a workspace version and target architecture in its filename. `scripts/package_macos_acceptance.sh <archive>` installs and exercises an archive in a temporary clean home. It relocates the install tree, completes the six-step tour from an empty data directory, initializes a fixture, runs an offline Arena comparison, replays the ledger, and opens both consoles with no host Node or npm on `PATH`. Browser acceptance checks complete static assets, comparison evidence against the CLI, token/Host/Origin guards, the read-only command boundary, fresh frozen bootstrap, and Ctrl+C while preserving the daemon.

## Install and launch

After obtaining an archive, extract it and run its installer. Installation is user-local by default and requires no administrator privileges. The installed CLI and daemon require a Git executable on `PATH`. Git is not bundled in this milestone: fixture initialization and deterministic execution use Git to create and inspect the private source worktree. A macOS setup without Git can install and inspect the package, but cannot initialize or run the quickstart fixture.

Before running a downloaded release, complete the checksum, Sigstore signature
and build-provenance checks in [release verification](RELEASES.md#verifying-a-downloaded-archive).
These establish the published archive's identity; macOS evaluates downloaded
executables separately.

```sh
tar -xzf hephaestus-v0.1.0-macos-arm64.tar.gz
cd hephaestus-v0.1.0-macos-arm64
./install.sh
export PATH="$HOME/.local/bin:$PATH"
hephaestus --version
heph
```

The installer creates a versioned release under `~/.local/share/hephaestus/releases`, switches the relative `current` link, and adds command symlinks under `~/.local/bin`. The installed tree can be moved as a unit. An alternate prefix is supported with `./install.sh --prefix /path/to/prefix`; existing unrelated executables or symlinks are left alone.

`heph` creates the quickstart project under the selected data directory, starts
the daemon frozen, and opens the guided tour. Enter advances each step, including
unfreezing and running the offline example. The Arena step records or reads its
metrics Selection; invariant verification and promotion remain separate. Quit
returns to the home menu. Use `heph stop` to stop the daemon, or `heph --tour` to
repeat the tour. A separate data directory can be selected with
`heph --data-dir /path/to/data`.

Run `heph web` for the browser console, or inspect an existing pilot:

```sh
heph web --data-dir /path/to/pilot/data --no-daemon
```

Open the exact local URL it prints. Under **Evidence & activity**, expand
**Agent identities and report** and choose **Download evidence report**.
`heph web` skips the Senate question and terminal tour. It starts a missing
daemon frozen without running agents. Ctrl+C closes the browser server;
`heph stop --data-dir /path/to/pilot/data` stops the remaining daemon.

When a daemon is already running, launching with `HEPHAESTUS_*` source/provider settings
requires stopping it first. The helper refuses to silently ignore those settings.
Use `--no-daemon` to attach with the existing daemon's configuration; that flag
does not apply new provider settings.
Empty or unchanged settings also require explicit attachment. Changes to
allowlisted credential values or `PATH` alone are not detected; restart to apply
them. See the copied provider setup guide for the complete handoff.

The acceptance script additionally needs Python 3 for its PTY driver. It tests with Git present while excluding host Node and npm from `PATH`; this does not prove operation without Git. The tour verifies replay before stopping its daemon; the separate reference Gauntlet verifies stop/restart recovery.

For a manual workflow, create the local quickstart source and configuration fixture:

```sh
hephaestus init --fixture quickstart ./hephaestus-quickstart
```

This copies the World template, Markdown Genomes, task manifests, and instructions into the new directory. It also creates and commits a small Git repository in a `repository` subdirectory; use that folder as `--source-repository` when starting `hephaestusd`. Register the World and Genomes with the installed `hephaestus` CLI, then use `run`, `arena evaluate`, `arena select`, `replay`, and `tui` as described by the copied README.

The package contains no hosted model credentials and does not invoke a paid provider. Its quickstart uses the bounded offline reference instruction language only. Promotion remains disabled until invariant evidence is joined to selection in a verified promotion decision.

For a representative hosted workflow, copy the support-triage pilot pack:

```sh
hephaestus init --fixture support-triage ./support-triage-pilot
```

Its copied README and provider-setup guide describe a private workspace, a frozen
daemon and `pilot prepare` with explicit provider/model/cost flags. Preparation
publishes the manifests and registers both prompt variants without calling a
provider. It works from this package without a checkout or host Node/npm/Python.
Actual evaluation requires your own provider CLI, authorized usage and explicit
authentication handoff. The cases are public and fictional; use independent
private holdouts for customer decisions.

## If macOS blocks a downloaded build

The locally tested ARM64 Hephaestus binaries have linker-generated ad-hoc
signatures. Apple Developer ID signing and notarization are not implemented in
the release workflow. macOS quarantine survives the documented
archive extraction and installation steps, so a downloaded build can require
an additional owner decision before launch.

Retain the exact warning and confirm the archive's release verification first.
For a warning that Apple cannot verify the developer or check whether software
is free of malware, Apple's
[instructions for opening downloaded software](https://support.apple.com/en-us/102445#openanyway)
describe the **System Settings → Privacy & Security → Open Anyway** option
after a launch attempt, where macOS offers it. Approve only the specific
verified build you intend to run. Stop and investigate messages reporting
detected malware, revoked authorization, or damaged or modified software.
If macOS offers no approval for the blocked command,
report the command, macOS version and exact warning through the
[issue tracker](https://github.com/randyren278/hephaestus/issues), or use the
[source installation](GETTING_STARTED.md).
A blocked daemon or helper can appear as an error from its parent command
without a macOS prompt; include that error when reporting the failure.

The [local quarantine evidence](evidence/2026-10-08-quarantine-probe/README.md)
records retained attributes, Gatekeeper assessment rejection and successful
package acceptance from the agent's process context. It does not establish
first launch from a published browser download on a fresh user's Mac.

## Platform and release limits

Archives are single-architecture. The installer checks that the archive architecture matches the current process architecture; install the matching `arm64` or `x86_64` archive. The acceptance script verifies install relocation and the offline fixture with Git present, while hiding host Node and npm. It is not proof of operation on a machine without Git. The release workflow is configured to publish on an explicit version tag, with Sigstore signatures and build provenance; see [release verification](RELEASES.md). Apple code signing and notarization are not implemented. Acceptance covers local archives and a [hosted rehearsal on both native architectures](evidence/2026-10-08-native-release-rehearsal/README.md); first launch after a quarantined browser download remains unverified.

## Local development: first-launch scan of new test binaries (macOS)

This affects building and testing from a source checkout, not the packaged release above. On macOS, every freshly linked binary (in particular, a `cargo test`/`cargo build` output that changed since it last ran) is scanned by the system before it is allowed to execute. The scan is often 30-90 seconds per binary and shows up as a process parked in `_dyld_start` at 0% CPU rather than a hang. A full local workspace test run can take several times longer than the equivalent CI run for this reason alone.

This is host security policy, not project code, so it cannot be fixed here. Two remedies:

- Add your terminal application under **System Settings -> Privacy & Security -> Developer Tools**. This exempts binaries launched *directly* by that terminal, but does not cover a binary spawned by a supervising process (for example, an agent or IDE task runner) that itself is not the terminal in that list.
- Run heavy suites (`cargo test --workspace`, mutation shards) from a Developer Tools-exempted terminal, or on Linux/CI, where this scan does not apply.

Tracked as TD-24 in `docs/dev/TECH_DEBT.md`: accepted as a host policy limitation, not fixed.
