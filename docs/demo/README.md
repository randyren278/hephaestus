# Recorded demo

`hephaestus-tui-demo.cast` is an [asciicast v2](https://docs.asciinema.org/manual/asciicast/v2/)
recording of one continuous 80x24 terminal session against the real
`hephaestusd` daemon, CLI, Arena, and Ink TUI — nothing mocked. It shows:

1. Starting the daemon against a fresh, isolated data directory.
2. Registering a World and a parent Genome through the CLI and seeding it as
   the World's Champion.
3. Opening the TUI and viewing **Lineage and Champions** to see the seeded
   Champion.
4. Using **Author Markdown agent** in the TUI to register a candidate Genome
   (pre-written to the TUI's default authoring path, standing in for an
   interactive `$EDITOR` session) and running a paired **Test** against the
   real Arena, showing a live visible score.

Recorded with `scripts/record_demo.py`, which drives a real PTY session with
Python (no `asciinema` dependency, since it may not be installed) and writes
the session as asciicast v2 JSON Lines.

## Replay

With [asciinema](https://asciinema.org/) installed:

```sh
asciinema play docs/demo/hephaestus-tui-demo.cast
```

Without it, the file is plain JSON Lines — the first line is the asciicast
header, and each following line is `[elapsed_seconds, "o", output_chunk]`; any
asciicast v2 player, or a small script that sleeps between chunks and writes
them to stdout, can replay it.

## Re-record

From a clean checkout, with the release binaries and TUI dependencies built:

```sh
cargo build --release --workspace
(cd apps/hephaestus-tui && npm ci)
python3 scripts/record_demo.py [output.cast]
```

The script builds its own scratch `$HOME` and daemon data directory under a
temp path, so it never touches your real `~/.hephaestus` state, and cleans
up on exit (success or failure). The only determinism shortcuts are
`$EDITOR=true` (a no-op, since the recorder cannot script an interactive
editor) and pre-writing the candidate Genome's Markdown source to the TUI's
default authoring path before the TUI opens it — the TUI still performs the
real World pick, real `$EDITOR` hand-off (a no-op edit), real
`genome_register` against the daemon's compiler, and a real paired
`evaluate_pair` Arena run. Output length in wall-clock time varies slightly
run to run because it waits on real daemon and Arena responses, not fixed
sleeps.
