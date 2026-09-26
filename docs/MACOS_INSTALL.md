# macOS package

**In short.** This page covers the ready-made macOS package: one download with everything inside, including the terminal console and its own copy of Node.js, so you don't need Rust, npm, or a source checkout. If you're installing from source instead, see [Getting started](GETTING_STARTED.md).


The packaging builder creates a relocatable, architecture-specific compressed tar archive for Apple Silicon (`arm64`) or Intel (`x86_64`). The package keeps `hephaestus`, the daemon, reference worker, evaluator, and process guardian together under `bin/`; the daemon and CLI resolve their helper binaries beside themselves. It also contains a bundled Ink TUI and a pinned Node.js 24.21.0 runtime. Running the installed CLI does not require a separate Node or npm installation.

## Build an archive

Build on macOS with Rust 1.85 or newer, Git, Python 3, Node/npm for TUI compilation, and the selected Rust target installed. The builder downloads the matching Node.js archive from the official Node distribution and verifies its pinned SHA-256 before including it. Those tools are build prerequisites; only Node is bundled for installed use.

```sh
python3 scripts/package_macos.py --arch arm64
# or, with the x86_64-apple-darwin Rust target installed:
python3 scripts/package_macos.py --arch x86_64
```

The Rust build uses a separate package-build target directory by default. The output archive is written under the target packages directory with a workspace version and target architecture in its filename. `scripts/package_macos_acceptance.sh <archive>` installs and exercises an archive in a temporary clean home. It relocates the install tree, initializes a fixture, runs an offline Arena comparison, replays the ledger, and opens the TUI with no host Node or npm on `PATH`.

## Install and launch

After obtaining an archive, extract it and run its installer. Installation is user-local by default and requires no administrator privileges. The installed CLI and daemon require a Git executable on `PATH`. Git is not bundled in this milestone: fixture initialization and deterministic execution use Git to create and inspect the private source worktree. A macOS setup without Git can install and inspect the package, but cannot initialize or run the quickstart fixture.

```sh
tar -xzf hephaestus-v0.1.0-macos-arm64.tar.gz
cd hephaestus-v0.1.0-macos-arm64
./install.sh
export PATH="$HOME/.local/bin:$PATH"
hephaestus --version
```

The installer creates a versioned release under `~/.local/share/hephaestus/releases`, switches the relative `current` link, and adds command symlinks under `~/.local/bin`. The installed tree can be moved as a unit. An alternate prefix is supported with `./install.sh --prefix /path/to/prefix`; existing unrelated executables or symlinks are left alone.

The acceptance script additionally needs Python 3 for its PTY driver. It tests with Git present while excluding host Node and npm from `PATH`; this does not prove operation without Git. It verifies explicit ledger replay within one daemon process, not daemon stop/restart recovery.

Create the local quickstart source and configuration fixture:

```sh
hephaestus init --fixture quickstart ./hephaestus-quickstart
```

This copies the World template, Markdown Genomes, task manifests, and instructions into the new directory. It also creates and commits a small Git repository in a `repository` subdirectory; use that folder as `--source-repository` when starting `hephaestusd`. Register the World and Genomes with the installed `hephaestus` CLI, then use `run`, `arena evaluate`, `arena select`, `replay`, and `tui` as described by the copied README.

The package contains no hosted model credentials and does not invoke a paid provider. Its quickstart uses the bounded offline reference instruction language only. Promotion remains disabled until invariant evidence is joined to selection in a verified promotion decision.

## Platform and release limits

Archives are single-architecture. The installer checks that the archive architecture matches the current process architecture; install the matching `arm64` or `x86_64` archive. The acceptance script verifies install relocation and the offline fixture with Git present, while hiding host Node and npm. It is not proof of operation on a machine without Git. The package is not signed or notarized by this repository, and CI does not publish GitHub Releases automatically. Use a trusted archive source and verify its SHA-256 before installation when distributing it outside the local acceptance flow.

## Local development: first-launch scan of new test binaries (macOS)

This affects building and testing from a source checkout, not the packaged release above. On macOS, every freshly linked binary (in particular, a `cargo test`/`cargo build` output that changed since it last ran) is scanned by the system before it is allowed to execute. The scan is often 30-90 seconds per binary and shows up as a process parked in `_dyld_start` at 0% CPU rather than a hang. A full local workspace test run can take several times longer than the equivalent CI run for this reason alone.

This is host security policy, not project code, so it cannot be fixed here. Two remedies:

- Add your terminal application under **System Settings -> Privacy & Security -> Developer Tools**. This exempts binaries launched *directly* by that terminal, but does not cover a binary spawned by a supervising process (for example, an agent or IDE task runner) that itself is not the terminal in that list.
- Run heavy suites (`cargo test --workspace`, mutation shards) from a Developer Tools-exempted terminal, or on Linux/CI, where this scan does not apply.

Tracked as TD-24 in `TECH_DEBT.md`: accepted as a host policy limitation, not fixed.
