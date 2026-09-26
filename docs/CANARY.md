# Drift, shadow, and canary control

Roadmap item 12: a living agent system must distinguish a poor agent from a
changed environment, and must adapt without risking the current Champion.
This slice adds three durable, replay-verified primitives on top of the
existing Champion, Forge, and Arena primitives: drift records, a canary's
shadow evaluation (the existing Forge assessment, reused), and a staged
canary rollout that promotes or rolls back only through the existing
`crates/hephaestus-control/src/champion.rs` promotion and rollback policy.

```sh
hephaestus drift record <drift-id> --world <world-id> --kind latency|cost|correctness|workload --evidence <evaluation-id>
hephaestus drift show <drift-id>

hephaestus canary start <canary-id> --world <world-id> --candidate <genome-id> --assessment <assessment-id>
hephaestus canary advance <canary-id> --evidence <evaluation-id>
hephaestus canary live-check <canary-id> --evidence <evaluation-id>
hephaestus canary show <canary-id>
```

## Drift records

A drift record cites one verified `SelectionReceipt` (the same receipt
`arena select` already produces) whose parent side is the World's current
Champion and whose candidate side is some other registered Genome. The
receipt's own parent-versus-candidate deltas are the "shift versus baseline":
the Champion is the baseline (parent), and the cited kind's delta must cross
a fixed, documented threshold before a record is admitted.

| Kind | Signal | Threshold |
| --- | --- | --- |
| `latency` | `candidate_latency_millis` vs `parent_latency_millis` | +20% (2000 bps) or worse |
| `cost` | `candidate_cost_microusd` vs `parent_cost_microusd` | +20% (2000 bps) or worse |
| `correctness` | `candidate_correctness_bps` vs `parent_correctness_bps` | -5% (500 bps) or worse |
| `workload` | `candidate_reliability_bps` vs `parent_reliability_bps` | -5% (500 bps) or worse (a reliable-trial-proportion drop is the signal used to detect workload-induced drift) |

`drift record` is refused if the evidence does not cross the documented
threshold for the requested kind, or if the World has no Champion yet. A
drift record **never** replaces a Champion; it is a fixed, hash-chained
`drift.recorded` event that startup, replay, and projection refresh
recompute and verify, and it is idempotent per `drift-id`.

## Shadow evaluation

Before starting a canary, an operator evaluates the candidate against the
current Champion using the existing pipeline: `arena evaluate`, `arena
select`, and `genome assess`. This produces a durable Forge assessment - the
canary's shadow evaluation. It is evidence-only: recording it never promotes
anything. `canary start` binds this assessment by ID and requires it to
exactly evidence the current Champion (as parent) against the candidate (as
child); the assessment's `metrics_passed`/`metrics_rejected` outcome is not
itself a gate here, because the staged health gates below and the existing
Champion promotion policy (still `metrics_passed`-gated) are the real safety
checks.

## Staged canary

`canary start` admits a canary at stage `pending`, capturing the World's
current Champion as `previous_champion_genome_id`. `canary advance` moves it
through `stage5` -> `stage25` -> `stage50` -> `completed`, one stage per
call, each gated by a fresh, verified `SelectionReceipt` pairing the previous
Champion (parent) against the canary's candidate (candidate) - the same
evidence orientation as drift. The prior Champion stays Champion at every
stage up to and including `stage50`.

A stage's health gate checks the same four dimensions as drift, using fixed
regression thresholds (latency/cost worse by 2000 bps, correctness/
reliability worse by 500 bps):

- **Healthy** evidence (no dimension crosses its threshold) advances to the
  next stage, or - from `stage50` - completes the canary by promoting the
  candidate through `champion::champion_transition_payload`'s existing,
  unchanged `Promote` policy, reusing the exact assessment bound at
  `canary start`. This is the only way a canary changes the Champion.
- **Regressed** evidence (any dimension crosses its threshold) **automatically
  aborts** the canary instead of advancing it - no operator action required.
  The Champion is never touched by an abort.

`canary advance` is refused while frozen, unless the evidence would abort
the canary: abort is a safety action, like Champion rollback, and remains
available while frozen.

## Live regression and automatic rollback

Once a canary has completed, `canary live-check` submits a fresh, verified
`SelectionReceipt` pairing the previous Champion (parent) against the live
Champion (candidate - the same orientation as staged advancement). If it
shows a regression by the same fixed thresholds, the daemon **automatically**
appends a `champion.transitioned` rollback event through
`champion::champion_transition_payload`'s existing, unchanged `Rollback`
policy - the same transition an operator could invoke by hand - restoring
the previous Champion and quarantining the regressed one. `canary
live-check` is allowed while frozen, and is refused (not silently a no-op)
when the evidence does not actually show a regression, so it cannot be used
to force an unwarranted rollback.

## Automatic drift-to-canary

A World whose `laws.auto_canary_on_drift` opted in (see
[WORLDS.md](WORLDS.md); default `false`, so an existing World's identity is
unaffected until it explicitly sets it) has every `drift.recorded` event
against it driven automatically, end to end, by the daemon's own
reconciliation loop (`ControlPlane::advance_drift_adaptations`, called every
`serve` tick alongside `advance_evolution`, never from a client connection).
For the oldest unfinished drift in such a World, one durable step per tick:

1. **Propose.** Forge proposes a mutation of the World's Champion at the
   moment the drift fired, through the ordinary `propose_genome_from_source`
   path (the same one `evolve` and `genome propose` use). If Forge's catalog
   offers no supported mutation for the Champion, the pipeline records a
   terminal `drift.adaptation_finished` with reason `no_candidate_mutation`
   and stops; nothing else happens.
2. **Shadow evaluation.** A paired Arena evaluation of the Champion against
   the proposed child, selected and Forge-assessed exactly like a manual
   `canary start`'s shadow evaluation.
3. **Staged canary.** `canary start` binds that assessment, then the
   pipeline submits one fresh paired evaluation per stage and calls the same
   `canary advance` policy an operator would: healthy evidence advances
   5% -> 25% -> 50% -> 100%; a regression at any stage **aborts** the canary
   (the Champion is never touched) and the adaptation finishes with reason
   `canary_aborted`; reaching 100% **completes** the canary through the
   unchanged Champion `Promote` policy and the adaptation finishes with
   reason `promoted`.
4. **Budget.** The whole pipeline is bounded by a fixed, documented budget of
   five paired evaluations (one shadow plus one per canary stage). It never
   runs a sixth trial for the same drift.

Every step is idempotent and driven solely from durable history — a daemon
restart mid-pipeline resumes exactly where it left off, exactly like
`evolve`. Freeze halts advancement (never clears an in-flight adaptation)
the same way it halts `evolve` and manual canary advancement. One
`drift.adaptation_started` event records the drift being adapted, the
World, the Champion at that moment, and the chosen proposal; one
`drift.adaptation_finished` event records why the pipeline stopped,
cross-referencing the proposal, shadow evaluation, assessment, canary, and
(when promoted) the Champion promotion transition it already produced,
rather than repeating their content. `verify_drift_adaptation_history`
recomputes and cross-checks every adaptation event against the history that
precedes it on startup, `hephaestus replay`, and every projection refresh,
and fails closed on a forged or out-of-order claim (for example, a
`drift.adaptation_finished` claiming `promoted` when no matching
`champion.transitioned` promotion exists in history). `drift show`/`drift
list` surface the adaptation's live status (`started`, `canary_id`,
`canary_stage`, `finished`, `finish_reason`) alongside the drift record
itself; the TUI and web console render it next to each drift.

The drift signal, the shadow evaluation, and every staged canary trial in
this pipeline all run on the deterministic reference runtime in the
in-process test suite (`crates/hephaestus-control/src/server_tests.rs`,
`auto_canary_on_drift_*`): a genuine correctness regression is recorded as a
drift, the reconciliation loop is drained (no client ever calls a
canary/Forge/Arena command directly), and the pipeline is shown proposing,
shadow-evaluating, staging through 5/25/50/100%, and promoting — or, for an
injected regression at a stage, aborting with the Champion left untouched.
Freeze-pause-then-resume and forged-claim rejection are also proven this
way. What this does **not** demonstrate: an automatic promotion or abort
driven by a real hosted-provider model's behavior, or a Forge mutation
chosen from anything beyond the existing catalog's supported operations —
the automatic trigger and staged rollout are real and tested, but the
candidate they adapt is still the same reference-runtime mutation
`evolve`'s unbound path proposes.

## Idempotency and identity

- A drift record's idempotency key is `drift-id`; its event ID is
  `drift:<drift-id>:recorded`, aggregated at `drift:<world-id>`.
- A canary's `started` event ID is `canary:<canary-id>:started`. Each
  `advance` or `abort` is keyed by `(canary-id, evidence-evaluation-id)` -
  `canary:<canary-id>:advance:<evaluation-id>` - so retrying the exact same
  advance call is idempotent, and a different pairing under an in-flight
  canary is rejected as a conflict. `live-check` uses the same pattern under
  `canary:<canary-id>:livecheck:<evaluation-id>`. All canary events share the
  aggregate `canary:<canary-id>`.
- Every drift and canary event is canonical JSON with `deny_unknown_fields`;
  startup, `hephaestus replay`, and every projection refresh recompute each
  event's expected payload from the history that preceded it (excluding, for
  a completing or rolling-back canary event, the one companion Champion
  event it also appended, which is verified separately by the existing
  Champion history check) and reject anything that does not match
  byte-for-byte. ID-prefix and event-type checks reject a retyped or
  reordered event exactly as the Champion history check does.

## What this does not do

- Drift records are observational by default; nothing automatically opens a
  canary from one unless the World's `laws.auto_canary_on_drift` opted in
  (see "Automatic drift-to-canary" above). Without that opt-in, an operator
  chooses the candidate and starts the canary by hand, exactly as before.
- A live regression can only be demonstrated end-to-end with genuinely
  independent evidence when the reference runtime's own measurements can
  differ between two evaluations of the same Genome pair - true today only
  for wall-clock latency, since correctness is a deterministic function of
  the reference-operation flip. The mechanism (automatic rollback trigger,
  idempotency, tamper and replay rejection) is fully implemented and tested
  against real evidence, including its guard rails (refusing a rollback when
  the evidence is healthy, or paired the wrong way, or the canary never
  completed); the in-process test suite does not additionally attempt to
  force a live regression through timing noise, because that would make the
  test flaky rather than more convincing.
