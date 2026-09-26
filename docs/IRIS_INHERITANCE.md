# Iris Inheritance

**In short.** From Iris, an earlier project, Hephaestus borrows one idea: a model may suggest what to do, but plain, predictable code decides whether it is allowed.


Hephaestus inherits Iris's authority principle: the model may propose intent, while deterministic infrastructure decides whether the action is allowed.

## Preserved ideas

- Explicit, scoped capabilities instead of ambient authority.
- Fail-closed validation at every trust boundary.
- Persistent freeze and kill controls owned by an external operator.
- Approval and denial decisions recorded as evidence.
- Child authority equal to or narrower than parent authority.
- Local daemon ownership of consequential transitions.

## Generalization

Iris asks whether a session may act. Hephaestus applies the same rule to whether a Genome may run, a mutation may touch a layer, an experiment may spend a budget, a candidate may enter sealed evaluation, or a promotion may proceed.

Hephaestus reuses the philosophy, not Iris source code. Shared libraries are considered only after both systems expose stable, benchmarked requirements.
