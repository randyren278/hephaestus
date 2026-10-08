# Getting started

**New here?** Hephaestus is a fair referee for AI agents. You give it two
versions of an agent. It runs both on the same tasks, each locked in its own
sandbox, and a judge they can't see scores them. It then saves the verdict in
a record that can't be quietly edited. If words like Genome, World, or Arena
are unfamiliar, keep [the glossary](TERMINOLOGY.md) open beside this page.

This is the first-run walkthrough for a source checkout. It covers install,
the `heph` launcher, the guided tour it opens with, and where to go once
you're past it. Prefer a prebuilt macOS package? See
[macOS installation](MACOS_INSTALL.md) instead — the rest of this page
assumes you're building from source.

## Prerequisites

- macOS (candidate/reference execution needs a verified OS sandbox; only
  Seatbelt is supported today — see [How it works](../README.md#under-the-hood)).
- `git`.
- A stable Rust toolchain, 1.88 or newer (`cargo --version`).
- Node.js 22 or newer with `npm`, to install and run the operator TUI from
  source. Clean source installation and the tour were verified with Node 22.

## Install

```sh
git clone https://github.com/randyren278/hephaestus.git
cd hephaestus
scripts/install.sh
```

`scripts/install.sh` builds `hephaestus`, `hephaestusd`, `heph`, and the
worker/evaluator/guardian helper binaries in release mode, installs the
operator TUI's npm dependencies, and symlinks the binaries into
`~/.local/bin` (pass `--prefix /some/other/path` to install elsewhere). It
never uses `sudo`. Build output stays in the checkout (or your chosen
`CARGO_TARGET_DIR`), dependencies use the usual Cargo/npm caches, and installed
links go into the chosen prefix. A second run rebuilds and relinks.

Run from a terminal, the installer first asks whether you're just here for
the Senate, the standalone debate tool. Answer yes, or pass `--senate-only`,
to install only that. See [the Senate](SENATE.md). Pass `--full` to skip the
question and install everything.

Make sure the prefix's `bin` directory is on your `PATH`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## Launch: `heph`

```sh
heph
```

The first time you run it against a fresh data directory (`~/.hephaestus` by
default, or `--data-dir <path>` to pick another one), `heph`:

1. Starts `hephaestusd` if one isn't already serving that data directory,
   bootstrapping the bundled quickstart fixture the same way
   `hephaestus init --fixture quickstart` does, if nothing better is
   configured.
2. Waits for the daemon to answer, then opens the operator TUI — forcing the
   first-run tour, since this data directory has never completed one.

Once the daemon is up, `heph` and the ordinary `hephaestus` CLI are talking
to the exact same canonical event ledger; anything the tour does is real,
receipted history, not a simulation. An operator who prefers each step by
hand can do everything `heph` does with `hephaestus` directly — see
[docs/CLI.md](CLI.md) for the full command reference.

On the very first run in a terminal, `heph` asks "Just here for the
Senate?" before starting anything. Answer yes and it shows you how to use
`senate` and starts nothing else. Answer no, or just press Enter, for the
full launch.

`heph stop` stops the daemon `heph` started. `heph --no-daemon` fails fast
instead of starting one, if you'd rather manage `hephaestusd` yourself.

## The first-run tour

<p align="center">
  <img src="assets/tour.svg" width="100%" alt="The six-step first-run tour. 1, welcome: what a Genome and a World are, and the promise that every claim carries a receipt. 2, your first World: registers the example World and a parent and changed child agent. 3, unfreeze and run: releases the brake and runs the parent once in a sandbox. 4, measure in the Arena: parent versus child on the same tasks, who won, by how much, and how sure. 5, prove it: rebuild everything from the record and check it matches live state. 6, done: where every screen lives, and how to replay the tour with heph --tour.">
</p>

The tour is six short, learn-by-doing steps against the real daemon. Every
step shows its position (`Step n of 6`), a one-sentence reason it matters,
and its live result — nothing is faked or pre-recorded:

1. **Welcome** — what a Genome and a World are, and the one promise this
   whole system makes: every claim carries a receipt in the canonical event
   ledger.
2. **Your first World and Genomes** — registers (or reuses) the bundled
   quickstart World and its parent/candidate Genomes.
3. **Unfreeze and run** — lifts the freeze every daemon starts under, then
   runs the parent Genome and shows its terminal state and trace count.
4. **Measure in the Arena** — runs parent versus candidate on the same
   visible tasks and shows the correctness delta, its confidence interval,
   and whether the candidate is eligible for promotion. Sealed tasks are
   never shown here, by design.
5. **Prove it** — replays the ledger from scratch and checks the replayed
   state matches live state exactly, the same guarantee `hephaestus replay`
   gives you at any time.
6. **Done** — where the rest of the operator console lives (Lineage,
   Evidence & Costs, Gene Bank, Drift/Canary/Meta-eval) and how to see this
   tour again.

Controls are shown at the bottom of every step: `Enter` for Next, `b` for
Back, `s` to Skip the rest of the tour, `q` to quit. Skipping or finishing
records a durable marker so the tour doesn't run again uninvited — replay it
any time with:

```sh
heph --tour
```

(or `hephaestus tui --tour`, if you're driving the CLI directly).

On a host with no verified OS sandbox, the run and Arena steps say so
plainly and explain what's still real (registration, replay, and
inspection) instead of pretending to succeed.

## After the tour

- [Support-triage acceptance pack](../examples/support-triage/README.md) —
  prepare a strict JSON task set and two hosted prompt variants without calling
  a model, then review the usage allowance before measuring them.
- [docs/CLI.md](CLI.md) — the full command reference: registering Worlds
  and Genomes, running, Arena evaluation and selection, replay, drift and
  canary rollouts.
- [docs/STATUS.md](STATUS.md) — what's built today.
- [apps/hephaestus-tui/README.md](../apps/hephaestus-tui/README.md) — the
  operator console's own screens, in more depth than the tour covers.
- The [Quickstart](../README.md#try-it-in-five-minutes) section of the README walks the
  same registration → run → Arena → replay loop by hand, one `hephaestus`
  command at a time; `scripts/quickstart.sh` runs it end to end against a
  scratch daemon and proves the state survives a restart.
