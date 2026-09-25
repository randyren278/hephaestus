# Gene Bank: extraction, transfer, and speciation

The Gene Bank turns one promoted, evidence-bound mutation into reusable
intelligence: a Gene is extracted only from a Champion promotion that clears a
deterministic minimum-evidence threshold, transferred onto other lineages
through the ordinary compiler exactly like Forge child compilation, measured
from a real paired evaluation, and only ever grouped into a specialist species
when the recorded evidence shows a persistent, statistically significant
domain advantage. Hera's contradiction philosophy is reused directly: a Gene
that measures positive in one lineage and negative in another is an explicit,
idempotent, never-overwritten record, not a number that gets averaged away.

```sh
hephaestus gene extract <gene-id> --promotion <champion-transition-id>
hephaestus gene transfer <trial-id> --gene <gene-id> --to <genome-id>
hephaestus gene record <trial-id> --evaluation <evaluation-id>
hephaestus gene show <gene-id>
hephaestus gene list
hephaestus gene speciate <species-id> --gene <gene-id> --domain <world-id>
```

## A Gene

A Gene is the minimal mutation the current reference language supports (a
`identity` <-> `ascii_uppercase` reference-operation flip), bound to its
origin evidence and World/domain scope. `gene extract` takes the exact
Champion transition ID a `champion promote` produced and re-verifies, by ID
and hash, the promotion's Forge assessment, the assessment's Forge proposal,
the proposal's verified child-selection event, and the promotion's invariant
event. It refuses:

- a transition that is not a `Promoted` kind (a seed or rollback carries no
  mutation evidence to extract from);
- a promotion whose verified selection receipt has fewer than
  `GENE_MIN_EVIDENCE_TRIALS` (3) measured paired trials — one lucky trial can
  never become a Gene.

Extraction records one idempotent `gene.extracted` event. Reusing a `gene-id`
with the same `promotion_transition_id` returns the original Gene; reusing it
with a different transition fails closed.

## Transfer trials

`gene transfer` applies the Gene's exact mutation to `--to <genome-id>`
through the ordinary Genome compiler — the same `compile_genome` call Forge
child compilation uses — producing an unevaluated child registered under the
recipient's World. The recipient must currently carry the Gene's exact origin
(pre-mutation) operation; a recipient that is already past that point, or
carries an unsupported prompt, is refused. This records one idempotent
`gene.transfer_applied` event, and (like `forge.proposed`) the child it
carries is a real registered Genome the Arena can evaluate.

`gene record` takes a verified paired evaluation and selection of the
recipient versus the transfer child (`arena evaluate` then `arena select`,
exactly as for any other pair) and classifies the measured effect from the
selection receipt's paired correctness-delta confidence bounds:

- **positive** — the lower confidence bound is above zero;
- **negative** — the upper confidence bound is below zero;
- **neutral** — neither bound crosses zero: no statistically significant
  effect either way.

This records one idempotent `gene.transfer_recorded` event binding the
transfer's applied event, the evaluation, and the verified selection receipt
by ID and hash. **Negative transfer is retained, never dropped**: there is no
code path that deletes or overwrites a recorded trial.

## Contradictions

The first time a Gene has both a recorded positive trial and a recorded
negative trial (in any lineages, any Worlds), the daemon automatically
appends one idempotent `gene.contradiction` event naming the first positive
and first negative trial by ID, hash, and domain (World). It is never
recomputed away and never overwritten by later evidence; `gene show` and
`gene list` surface it. This is the same "surface the conflict, don't
silently pick a side" discipline this vault's Hera contradiction pipeline
uses.

## Speciation

`gene speciate <species-id> --gene <gene-id> --domain <world-id>` admits a
specialist species only from the domain's own recorded transfer evidence:

1. **Persistent**: zero recorded negative transfers in that domain. One
   recorded negative is enough to refuse, regardless of how many positives
   exist.
2. **Statistically significant**: at least `SPECIATION_MIN_LINEAGES` (3)
   distinct recipient Genomes with a recorded *positive* trial in that domain.
   Neutral trials do not count toward this.
3. A mean measured effect (paired correctness delta estimate) across those
   positive trials of at least `SPECIATION_MIN_EFFECT_BPS` (300 basis points,
   3%).

Any unmet condition is refused with an explicit, specific reason (not a
generic denial). Speciation records one idempotent `gene.species_created`
event binding the supporting lineage Genome IDs and transfer-recorded event
IDs. These thresholds are deterministic constants in
`crates/hephaestus-control/src/gene_bank.rs`; changing them is a documented,
reviewable code change, not runtime configuration.

## Canonical events and replay

Every Gene Bank event — `gene.extracted`, `gene.transfer_applied`,
`gene.transfer_recorded`, `gene.contradiction`, `gene.species_created` — is
canonical JSON with a deterministic ID derived from its idempotency key
(`gene:<gene-id>:extracted`, `gene:transfer:<trial-id>:applied`, and so on),
exactly like Champion transitions in `crates/hephaestus-control/src/champion.rs`. `verify_gene_bank_history`
recomputes each event's payload from the exact history that preceded it on
daemon startup, explicit `replay`, and every projection refresh, and rejects:

- a payload that differs from what the same inputs recompute to;
- a non-canonical payload encoding;
- an event retyped to hide its origin — detected by ID-and-aggregate-prefix,
  not event-type alone, exactly like Champion history;
- events reordered so a record precedes the trial it depends on.

## What this does not do yet

The only supported mutation is the reference-operation flip, so Gene extent
is bounded to that one change, same as Forge. `gene transfer` requires the
recipient to be an exact reference-instruction match for the Gene's origin
operation; there is no partial or fuzzy transfer. Domain is exactly "the
recipient Genome's registered World" — there is no richer domain taxonomy
(e.g. task category) yet. Speciation creates a durable record but does not
yet change how the daemon evolves or evaluates within that domain; using a
species to bias future Forge proposals or Arena scoring is future work.
