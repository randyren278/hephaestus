# The Hephaestus Gauntlet (current slice)

Roadmap item 10 and master-plan L24 describe a public benchmark suite
"specifically designed to force harness adaptation," naming seven adversarial
failure modes: **context loss, premature completion, schema drift, bad
routing, duplicate subagents, poisoned memory, and hallucinated
verification.**

## Honest coverage: 0 of 7 named failure modes are expressible today

The only runtime `hephaestus evolve` can drive is the offline deterministic
reference worker described in [docs/GENOMES.md](../../docs/GENOMES.md#offline-reference-instruction-subset):
one fenced `hephaestus-reference-v1` instruction selecting `identity` or
`ascii_uppercase`, applied to one bounded byte string, with no tool calls, no
multi-turn context, no subagents, and no persisted memory. None of the seven
named modes can be *genuinely* reproduced by that worker; building a fixture
that merely resembles one in name would misrepresent what the Gauntlet
actually proves. The honest per-mode gap:

| Failure mode | Expressible today? | Runtime capability required |
|---|---|---|
| Context loss | No | A multi-turn context window with bounded history/compaction. The reference worker has no conversation state at all: every task is one isolated input-to-output call. |
| Premature completion | No | A multi-step task representation with an intermediate "done" signal and a verification gate that can catch a claim made before real completion. The reference worker either returns a complete transform or fails a hard budget; there is no partial-progress state to prematurely abandon. |
| Schema drift | No | A real tool-calling adapter with a versioned tool/function schema that can change shape underneath a working harness. The reference worker has no tools; its only "schema" is the two-field `hephaestus-reference-v1` instruction envelope, which cannot drift independently of the Genome that declares it. |
| Bad routing | No | A model/provider routing table with more than one selectable route and a cost or capability signal to route on. The reference worker is the only executable route; a Genome's `model` provider and family are recorded but only the deterministic reference pair ever runs (see [docs/GENOMES.md](../../docs/GENOMES.md)). |
| Duplicate subagents | No | A subagent spawning/orchestration capability with an observable agent graph. The reference worker runs as a single isolated process per trial; there is no notion of a subagent, let alone two of them. |
| Poisoned memory | No | A persistent memory/experience retrieval store that a candidate reads from across runs, plus a provenance policy to poison. Reference trials are stateless and isolated; nothing persists between one task execution and the next for a candidate to read. |
| Hallucinated verification | No | A runtime whose own self-reported completion status can diverge from ground truth (in practice, a hosted-model adapter that can claim success it did not earn). The reference worker's `RunCompletionReason` is produced by our own deterministic code, not a model claim, so it cannot hallucinate; the trusted Arena evaluator, not the candidate, always computes the real correctness score independently. |

Every one of the seven needs a runtime and/or adapter capability roadmap
items 5, 6, 11, 14, and 19 are meant to eventually supply (sandboxed
multi-provider adapters, full trace/context provenance, the Gene Bank and
transfer engine, the MCP gateway, and the drift engine). Building any of them
into the deterministic reference worker directly would blur the "deterministic
local reference runtime" line the roadmap and README both hold as load-bearing,
so this slice does not attempt it.

## What *is* expressible today: sealed-holdout generalization

The one property every later, richer Gauntlet World will still need is
already fully real: **a candidate that looks strictly better on visible tasks
can still regress on sealed ones**, and the deterministic promotion policy
(`maximum_regressions: 0` in the fixture World below) must catch that before
a human or an autonomous run promotes it. [`sealed-holdout/`](sealed-holdout/)
is a fixture World proving exactly this with the two real reference
operations:

- `identity` returns its input unchanged.
- `ascii_uppercase` uppercases ASCII input.

Both visible tasks give lowercase input with an uppercase expected output, so
`ascii_uppercase` looks like an unambiguous, uniform improvement over
`identity` if you only look at what a candidate is allowed to see. The sealed
manifest adds a task whose expected output is a **pre-formatted mixed-case
string equal to its own input** (`sealed-already-formatted`): `identity`
passes it for free, but `ascii_uppercase` mangles it and fails. Selection
evidence over the combined visible-plus-sealed set therefore reports one
genuine paired regression alongside three improvements
(`correctness_regressions=1`, `correctness_improvements=3`,
`metrics_eligible=false`), and `champion promote`/an unattended `evolve` run
correctly refuses to promote past it under this World's zero-regression
policy (`arena invariants` is a separate, optional evidence check this
fixture does not need: promotion is already blocked by the selection
receipt's own regression count).

| File | Registered as |
|---|---|
| `tasks/visible.json` | `arena.visible_manifest` |
| `tasks/sealed.json` | `arena.sealed_manifest` |
| `invariants.json` | `arena.invariant_manifest` (required: an unattended `evolve` run always checks invariants before assessing a child) |
| `world.template.json` | the World (`__VISIBLE_MANIFEST__`, `__SEALED_MANIFEST__`, `__EVALUATOR__`, `__VERIFIER__`, `__INVARIANTS__` are the addresses the daemon prints for the manifests, the evaluator binary, `hephaestus verifier`, and the invariant manifest, exactly as in [examples/quickstart](../quickstart/README.md)) |
| `parent.md` | root Genome (`identity`) |
| `candidate.md` | child Genome (`__PARENT_ID__` becomes the registered parent's identity; `ascii_uppercase`) |

### Running it

Register the World and both Genomes the same way
[`scripts/quickstart.sh`](../../scripts/quickstart.sh) does, pointing at this
directory instead of `examples/quickstart`, plus one extra `artifact put
invariants.json` step substituted into `__INVARIANTS__`. Then either:

- drive it manually — `arena evaluate`, `arena select`, `genome propose`,
  `genome assess`, `champion seed`/`promote` — and observe that `arena select`
  reports `correctness_regressions=1` and `metrics_eligible=false` for the
  parent-versus-candidate pair; or
- seed the parent as Champion and run
  `hephaestus evolve start gauntlet-1 --world <world-id> --from <parent-id> --generations 3 --budget 6`
  (see [docs/EVOLUTION.md](../../docs/EVOLUTION.md)) and poll
  `hephaestus evolve status gauntlet-1`: because this World's baseline
  comparison Genome is the `ascii_uppercase` candidate itself, generation
  zero's diagnostic step and its proposed mutation still exercise the same
  identity/uppercase flip, and every generation's assessment fails the
  World's zero-regression policy the same way — verified end to end against a
  real daemon while building this fixture, all three generations complete
  with `promoted=false` and the run finishes with `GenerationsExhausted`,
  never promoting past generation zero's Champion. That outcome is itself the
  point: the sealed holdout is doing its job.

## Extending this Gauntlet honestly

When a future roadmap item adds one of the missing capabilities above (a
real tool-calling adapter, a context/compaction mechanism, subagent
orchestration, a memory store, or a routing table), add a new fixture World
under `examples/gauntlet/<mode>/` that names the specific failure mode it
reproduces, and update the coverage table in this file in the same commit.
Until then, this directory's honest claim is exactly one property: sealed
holdout evidence catches a regression a visible-only view would miss.
