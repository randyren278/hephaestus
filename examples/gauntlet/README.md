# The Hephaestus Gauntlet

Roadmap item 10 and master-plan L24 describe a public benchmark suite
"specifically designed to force harness adaptation," naming seven adversarial
failure modes: **context loss, premature completion, schema drift, bad
routing, duplicate subagents, poisoned memory, and hallucinated
verification.**

## Honest coverage: all 7 modes are expressible as deterministic proxies

The only runtime `hephaestus evolve` can drive is the offline deterministic
reference worker described in
[docs/GENOMES.md](../../docs/GENOMES.md#offline-reference-instruction-subset).
That worker has no real conversation state, tool calls, provider routing,
subagents, or persisted memory — nothing about it changed. What changed is
that the worker now accepts 14 additional operations
(`crates/hephaestus-runtime/src/reference_instruction.rs`), one bad/fix pair
per named mode, each of which parses a small strict JSON scenario from the
task input and computes a byte-exact output that either exhibits or avoids
the named pathology. This is honestly a **simulation embedded in the
reference worker**, not a real model, tool schema, router, subagent
orchestrator, memory store, or self-reporting model. It proves something
real and narrow: given a scenario shaped like the failure mode, a
deterministic "bad" transform genuinely produces the wrong answer and a
deterministic "fix" transform genuinely produces the right one, and the
Arena's trusted evaluator (not the candidate) is what tells them apart.

| Failure mode | Bad operation | Fix operation | What the scenario encodes |
|---|---|---|---|
| Context loss | `context_loss_naive` | `context_loss_aware` | A JSON array of conversation turns; a fact is stated in an early turn (`FACT: ...`) and the last turn asks for it. The bad operation only reads the last turn (as if the context window had already dropped the earlier one) and returns `UNKNOWN`; the fix scans every turn. |
| Premature completion | `premature_completion` | `verified_completion` | A JSON array of `"stepN:MARKER"` steps. The bad operation reports the first step's marker alone, as if it declared victory after one step; the fix requires every step's marker before reporting completion. |
| Schema drift | `schema_drift_brittle` | `schema_drift_adaptive` | A JSON object with a `schema_version` and the value under a version-specific field name (`field_v1`/`field_v2`). The bad operation always reads the v1 field name even once the schema has drifted to v2 (returning `MISSING_FIELD`); the fix reads whichever field the declared version names. |
| Bad routing | `bad_routing_cheapest` | `capability_aware_routing` | A JSON list of routes, each with a `capability` and `cost`, plus the task's `requires_capability`. The bad operation always picks the cheapest route regardless of capability match; the fix picks the cheapest route that can actually serve the request. |
| Duplicate subagents | `duplicate_subagents_wasteful` | `deduplicated_subagents` | A JSON list of subagent task requests with exact duplicates. The bad operation executes every request, including duplicates; the fix deduplicates them first, preserving first-seen order. |
| Poisoned memory | `poisoned_memory_trusting` | `provenance_checked_memory` | A JSON list of memory entries, each with `text` and a `trusted` provenance flag, ending in an untrusted (poisoned) entry. The bad operation returns the most recent entry regardless of trust; the fix returns the most recent *trusted* entry. |
| Hallucinated verification | `hallucinated_verification_trusting` | `ground_truth_verification` | A JSON object with a self-reported `claimed_output`/`claimed_status` and a separate `actual_state`. The bad operation trusts a `"success"` status and echoes the claim even when it disagrees with the actual state; the fix always reports the actual state. |

Every pair is exercised by
`crates/hephaestus-runtime/src/reference_instruction.rs`'s
`gauntlet_tests` module (one unit test per mode, checking the exact bytes
each operation produces) and, at the Arena level, by
`hephaestus-control`'s `gauntlet_failure_modes_reject_the_bad_operation_and_pass_the_fix`
test: for each mode it registers a fixture World with a `parent` Genome
carrying the bad operation and a `candidate` Genome carrying the fix, runs a
real paired Arena evaluation, and asserts the parent scores zero correctness
while the candidate scores full correctness. `examples/gauntlet/<mode>/`
(one directory per mode: `context-loss/`, `premature-completion/`,
`schema-drift/`, `bad-routing/`, `duplicate-subagents/`,
`poisoned-memory/`, `hallucinated-verification/`) holds the matching
World/Genome/task fixtures for a manual walkthrough, structured exactly like
[`sealed-holdout/`](sealed-holdout/) below: `world.template.json`,
`invariants.json`, `parent.md`, `candidate.md`, `tasks/visible.json`,
`tasks/sealed.json`.

### What this now also proves: `evolve` can discover a mode's fix on its own

Forge's supported mutations are now a deterministic catalog of all 16
reference-runtime operations (`crates/hephaestus-runtime/src/mutation_catalog.rs`),
not only the `identity`⇄`ascii_uppercase` flip, and a World's
`mutation_scope` must authorize `harness` mutations before Forge will
propose one at all (every fixture below already does). When an `evolve` run
binds a registered Evolver strategy (`evolve start --strategy <id>`), each
generation's failure-cluster analysis (`failure-cluster-v2`) recognizes a
Champion running one of these seven modes' "bad" operations and suggests
its paired fix for *every* failure cluster, regardless of shape — because,
unlike the casing pair, a Gauntlet bad/fix pair's outputs usually share no
structural resemblance at all. `crates/hephaestus-control`'s
`evolve_promotes_the_fix_for_every_gauntlet_mode_from_the_bundled_fixtures`
test seeds each mode's bad Genome as Champion, its fix Genome as the fixed
diagnostic baseline, starts a one-generation strategy-bound `evolve` run,
and asserts the run promotes a child carrying exactly that mode's paired fix
operation through the unmodified Arena/selection/invariant/Champion policy —
proving `evolve` can now *discover* a fix for each named mode on its own, not
only replay an operator-supplied one.

### What this does *not* yet prove

A strategy-bound generation still proposes exactly one candidate mutation
(the highest-priority failure cluster's suggestion, optionally reordered by
a Gene Bank preference); it does not propose and rank several candidates per
generation (`EvolverStrategyConfig.candidate_count` above `1` is recorded
but not yet actionable — see `TECH_DEBT.md` TD-17). The catalog is a closed
table of these exact 16 operations, not free-form prompt editing: Forge
cannot propose, and the reference runtime cannot execute, any operation
outside it. And the reference runtime remains what it always was — a
deterministic simulation of each failure mode's shape, not a real
multi-turn model, tool schema, router, subagent orchestrator, memory store,
or self-reporting model (see above).

## What else is expressible today: sealed-holdout generalization

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

## Statistically supported improvement on a sealed holdout: `sealed-holdout-improvement/`

`sealed-holdout/` above proves the *regression* half of the sealed-holdout
property: a candidate that looks like an improvement can fail on tasks it
never saw. [`sealed-holdout-improvement/`](sealed-holdout-improvement/) proves
the other half — the one roadmap item 10 actually asks for — that a genuine
improvement can be *discovered and statistically supported* against a sealed
holdout the candidate never saw during development, not just asserted.

The fixture reuses the poisoned-memory pair from the table above: the
`poisoned_memory_trusting` parent Genome is seeded as Champion, and 3 visible
plus 8 sealed poisoned-memory scenarios (distinct memory content per
scenario, none shared with `examples/gauntlet/poisoned-memory/`) are
registered as the World's visible and sealed manifests. The bad operation
fails every one of the 11 scenarios; `provenance_checked_memory` passes every
one. `crates/hephaestus-control/src/server_tests.rs`'s
`sealed_holdout_improvement_is_statistically_supported_and_promoted_within_budget`
starts a one-generation, two-trial-budget strategy-bound `evolve` run from
that Champion and asserts:

- the run promotes the fix within its enforced budget (`trials_consumed <=
  2`) and the promoted child carries `provenance_checked_memory`;
- the child's selection receipt reports zero correctness regressions, zero
  unchanged, and 11 improvements, with a bootstrap lower confidence bound
  that clears zero at the World's 95% confidence and `metrics_eligible=true`;
- reading the promoted child's operator evaluation directly, the *sealed*
  subset alone moved from 0/8 to 8/8 correct — the improvement is real on
  tasks the candidate never had visibility into, not just the visible set;
- replay still verifies; and
- an independent Python recompute
  (`hephaestus_lab.crosscheck`, see [docs/LAB_CROSSCHECK.md](../../docs/LAB_CROSSCHECK.md))
  of the exact same receipt agrees with the Rust bootstrap.

This closes roadmap item 10's last gap: a measured, not asserted, statistically
supported improvement on a sealed holdout, discovered by the unmodified
Arena, selection, and invariant policy through the same mutation-catalog
machinery described above — no new production code, only a fixture and a
proof.

## The bundled `coding` World and `hephaestus evolve coding`

[`coding/`](coding/) is the World `hephaestus evolve coding --budget <n>`
registers and drives automatically (see
[docs/EVOLUTION.md](../../docs/EVOLUTION.md)). It uses the same
`identity`/`ascii_uppercase` pair as `sealed-holdout/`, applied to short
Python-function-shaped visible and sealed tasks so the World reads as
"coding"-flavored; `evolve coding` starts no strategy, so its run always
uses the historical casing-flip default described above, even though Forge
could now propose any of the 16 catalog operations for a strategy-bound
run. Running it proves the unattended multi-generation *mechanism* end to
end from one command — registration, three generations, completion —
against a Gauntlet-named World; it does not prove the optimizer can solve a
real coding task, and it does not exercise any of the seven named
failure-mode operations above (`evolve coding` is a fixed convenience
command with no `--strategy` flag of its own; see `hephaestus evolve start
--strategy <id>` above for the mechanism that does reach them).

## Extending this Gauntlet honestly

The seven failure-mode proxies above are intentionally simple: one JSON
scenario, one byte-exact bad/fix pair, proven only through direct Arena
evaluation. When a future roadmap item adds a real capability (a real
tool-calling adapter, a context/compaction mechanism, subagent orchestration,
a persisted memory store, or a routing table), prefer building the richer
version alongside these proxies rather than deleting them, and update this
file's claims in the same commit. Until then, this directory's honest claims
are exactly three: each named failure mode has a genuine deterministic proxy
that Arena evaluation alone can reject/pass; a strategy-bound `evolve` run
can discover each mode's fix on its own through Forge's closed 16-operation
mutation catalog (not free-form prompt editing); sealed-holdout evidence
catches a visible-only regression that a richer Gauntlet World will still
need to catch; and a genuine improvement discovered by a strategy-bound
`evolve` run is statistically supported on a sealed holdout the candidate
never saw, cross-checked by an independent Python recompute of the same
receipt.
