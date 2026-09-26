# Getting started

This is the first-run walkthrough for a source checkout. It covers install,
the `heph` launcher, the guided tour it opens with, and where to go once
you're past it. Prefer a prebuilt macOS package? See
[macOS installation](MACOS_INSTALL.md) instead — the rest of this page
assumes you're building from source.

## Prerequisites

- macOS (candidate/reference execution needs a verified OS sandbox; only
  Seatbelt is supported today — see [How it works](../README.md#how-it-works)).
- `git`.
- A stable Rust toolchain, 1.85 or newer (`cargo --version`).
- `npm`, to install the operator TUI's dependencies. No separate Node
  install step is needed beyond that — the TUI runs from source.

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
never uses `sudo` and never writes outside your checkout's own `target/`
directory and the chosen prefix, so it's safe to re-run — a second run just
rebuilds and relinks.

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

`heph stop` stops the daemon `heph` started. `heph --no-daemon` fails fast
instead of starting one, if you'd rather manage `hephaestusd` yourself.

## The first-run tour

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

- [docs/CLI.md](CLI.md) — the full command reference: registering Worlds
  and Genomes, running, Arena evaluation and selection, replay, drift and
  canary rollouts.
- [docs/STATUS.md](STATUS.md) — what's built today.
- [apps/hephaestus-tui/README.md](../apps/hephaestus-tui/README.md) — the
  operator console's own screens, in more depth than the tour covers.
- The [Quickstart](../README.md#quickstart) section of the README walks the
  same registration → run → Arena → replay loop by hand, one `hephaestus`
  command at a time; `scripts/quickstart.sh` runs it end to end against a
  scratch daemon and proves the state survives a restart.
