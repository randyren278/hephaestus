# TUI theme and motion

The operator TUI (`apps/hephaestus-tui`) draws from the same palette as the
README hero image and the pixel-art sprites: a torch-lit colosseum where an
ORANGE-crested Champion faces a GRAPHITE-crested challenger under a
hooded judge holding a sealed scroll. Every screen in the console is built
from that one palette and a small set of shared motion primitives, so the
terminal experience and the project's visual identity stay one thing rather
than two.

The palette, roles, and glyphs live in `apps/hephaestus-tui/src/theme.ts`.
The motion primitives live in `apps/hephaestus-tui/src/motion.tsx`. Both are
plain source files worth reading directly; this page is the map, not a
substitute.

## Palette

The seven hex values are copied verbatim from `scripts/pixel_art/generate.py`
(`PALETTE`), which is also what renders `docs/assets/pixel/tui-banner.svg` and
the other pixel-art sprites, so re-running that script and this file's colors
never drift apart.

| Token         | Hex       | Reads as                                    |
|---------------|-----------|----------------------------------------------|
| `bg`          | `#10131a` | Base background (unused as a foreground)      |
| `bgRaised`    | `#181c26` | Raised panel background                       |
| `bgCard`      | `#1e2330` | Card background                               |
| `ink`         | `#e9e4d8` | Primary text                                  |
| `inkDim`      | `#a2a7b5` | Secondary / graphite text                     |
| `ember`       | `#e8590c` | The Champion's crest                          |
| `emberBright` | `#ff8a3d` | Champion highlight, torch flame, "success"    |
| `gold`        | `#f4c542` | The judge, the sealed evaluator, selection    |
| `gray`        | `#5b6270` | The challenger's graphite crest                |
| `danger`      | `#c0392b` | Regressions, aborts, rollbacks                |
| `border`      | `#3a3f4d` | Panel borders and rules                       |

## Role colors

Screens never reference a raw palette token or an Ink color name directly —
every `<Text>`/`<Box>` color comes from `theme.color(role)`, where `role` is
one of:

| Role            | Token         | Used for                                              |
|-----------------|---------------|--------------------------------------------------------|
| `champion`      | `ember`       | The Champion/candidate lane, Champion lineage rows     |
| `championBright`| `emberBright` | Torch flame highlight, progress-bar highlight          |
| `challenger`    | `gray`        | The parent/standby lane                                |
| `challengerDim` | `inkDim`      | Dimmer challenger text, the banner's still torch pixel |
| `judge`         | `gold`        | Headings, the judge's seal, selection markers          |
| `sealed`        | `gold`        | Receipt-style ids (run/genome/evaluation ids)          |
| `danger`        | `danger`      | Aborts, rollbacks, quarantined rows                    |
| `regression`    | `danger`      | A regression in evidence/cost/gene screens             |
| `success`       | `emberBright` | A positive outcome                                     |
| `improvement`   | `ember`       | An improvement in evidence/cost/gene screens           |
| `ink`           | `ink`         | Primary foreground text                                |
| `inkDim`        | `inkDim`      | De-emphasized text                                     |
| `border`        | `border`      | Panel borders                                          |
| `muted`         | `gray`        | Neutral / unselected / "nothing to report" text        |

Screen by screen:

- **Home**: the pixel banner (below), a typewriter-revealed `HEPHAESTUS`
  title, and a status dot where `FROZEN` reads `judge`, `RUNNING` reads
  `success`, and a stale connection reads `danger`. The menu's selected row
  gets the `champion` role and the shared caret glyph; unselected rows read
  `inkDim`.
- **Arena progress**: the parent lane reads `challenger`, the candidate lane
  reads `champion`; the trial progress bar's color follows whichever lane the
  job's current phase is in, and turns `judge` (with the seal glyph) while
  scoring or committing. A crest-clash idle animation plays while waiting for
  an evaluation id; a celebration burst plays once the visible score favors
  the candidate.
- **Lineage**: Champion rows read `champion` with the `▲` crest glyph,
  standby rows read `ink`, quarantined rows read `danger` with `✕`.
- **Evidence & Costs**: ids (run/genome/evaluation ids) read `sealed` (gold),
  matching the judge's seal on a receipt; a positive outcome
  (`promotion_eligible`, satisfied invariants, a passing Forge assessment)
  reads `improvement`, a negative one reads `regression`.
  Prompt diffs in the Genome detail panel use the same pair: added lines
  `improvement`, removed lines `regression`.
- **Gene Bank**: a small double-helix glyph accent (`⟋⟍`, alternating
  `champion`/`challenger`) sits next to the panel titles; gene ids read
  `sealed`, positive/negative transfer counts read `improvement`/`regression`.
- **Drift, Canary & Meta-eval**: the canary detail panel draws a 5/25/50/100
  stage rail, lighting each reached stage `champion` and the current stage
  `judge`; an aborted canary's stage label flashes `danger` before settling.

## Motion

Every animated primitive in `apps/hephaestus-tui/src/motion.tsx` is driven by
one process-wide ticker: at most one `setInterval`, at 100ms (~10fps),
started only while at least one animated component is mounted and stopped
the instant the last one unmounts. Adding more animated widgets on screen
never adds more timers — each one just asks for its own logical frame rate
(`useFrame(intervalMs, …)`) derived from that single shared tick.

The primitives:

- **`TorchFlicker`** — a torch flame that cycles glyph and color through
  ember → ember-bright → gold, used for the home banner's lit torch.
- **`Typewriter`** — reveals a header's text once, left to right, then holds;
  it never loops.
- **`ProgressBar`** — filled ember segments with a highlight that sweeps
  across the filled portion while live.
- **`Spinner`** — a braille spinner in the judge's gold.
- **`CrestClash`** — two crest glyphs (challenger `◆`, champion `▲`)
  approach, spark gold, and retreat — the idle animation for a "waiting on
  the daemon" state.
- **`CelebrationBurst`** — a ~1.2 second spark burst in gold/ember, played
  once for a promotion or when a candidate's visible score pulls ahead.
- **`DangerFlash`** — flashes red a few times before settling, for an abort
  or a rollback.
- **`GeneHelix`** — the Gene Bank's twisting double-helix accent.

Every primitive takes `animate` and `frame` props. Passing a `frame` number
freezes it at that exact point with no timer at all — how the deterministic
tests in `apps/hephaestus-tui/test/motion.test.tsx` and the view tests work.
Passing `animate={false}` renders the primitive's resting/final state, also
with no timer.

## Accessibility and fallbacks

`apps/hephaestus-tui/src/theme.ts` resolves one of three color levels before
any screen renders, from the environment alone:

- **`truecolor`** — `COLORTERM=truecolor`/`24bit`, or a `direct`-color `TERM`
  — the hex values above render as-is.
- **`basic`** — any other real terminal (a live TTY with a non-`dumb` `TERM`)
  — a curated 16-color approximation is used instead (`champion` → ANSI
  red, `judge` → ANSI yellow, and so on).
- **`none`** — plain text, no escape codes at all. This is what a piped
  command, a CI log, a test run, or `NO_COLOR` gets: `NO_COLOR` (per
  <https://no-color.org>) always wins unless the operator explicitly sets
  `FORCE_COLOR`, and the process not being attached to a TTY degrades the
  same way even if the environment otherwise looks colorful.

Motion follows the same idea but simpler: every primitive's shared ticker
only runs while the process is attached to a TTY (see `useMotionEnabled` in
`apps/hephaestus-tui/src/motion.tsx`). Piped output, CI, and the test suite
all get the primitives' plain resting frame — full text, a still torch, no
spinning — instead of a timer nobody can see tick.
