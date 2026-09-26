# Hephaestus Constitution

**In short.** These are the eight rules that no agent, model, or feature may break. Everything else in Hephaestus can change over time, but only inside these lines.

<p align="center">
  <img src="assets/constitution.svg" width="100%" alt="The eight Laws in plain English. 1, a released Genome can never change. 2, a child never gets more permissions than its parent. 3, agents can't touch the rules, the judge, the record, the budget, or the controls. 4, a model may recommend a promotion but never perform one. 5, every accepted change is recorded and can be replayed. 6, results from different Worlds are never compared as if they were the same. 7, freeze and kill always stay with the human operator. 8, anything malformed or unsupported is refused.">
</p>

The rest of this page is the precise reference.

Version 1 defines the trust rules that every runtime, storage backend, evaluator, and interface must obey. Product behavior may evolve only inside these boundaries.

## Purpose

Hephaestus exists to prove that an agent harness became better. A candidate's claim, a higher unpaired score, or a successful self-modification is not proof. Improvement requires attributable observations, an explicit hypothesis, protected evaluation, deterministic selection, and a reproducible evidence receipt.

## The Laws

1. Released Genomes and their content-addressed artifacts are immutable.
2. A child may inherit equal or narrower authority, never broader authority.
3. Candidates cannot modify Laws, evaluators, sealed holdouts, ledger integrity, budget enforcement, promotion policy, rollback, artifact hashes, World definitions, or operator controls.
4. Models may recommend promotion but cannot execute it.
5. Every accepted state transition is ledgered and replayable.
6. Incompatible Worlds are never presented as directly comparable.
7. Freeze and kill remain under external operator control.
8. Malformed or unsupported requests fail closed.

The first executable forms of these Laws are in `crates/hephaestus-core/src/authority.rs` and `crates/hephaestus-core/src/domain.rs`. CI deliberately weakens them and requires the suite to fail.

## Evolvable surface

Prompts, model routing, context, memory, retrieval, tools, planning, topology, verification, recovery, budgets below their ceilings, and the improvement strategy may evolve when a World permits it.

## Change control

A change to a Law creates a new constitutional and World version. Historical evidence remains attached to its original version. No migration may rewrite old events to make them appear governed by new Laws.
