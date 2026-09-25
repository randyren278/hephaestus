# Self-dogfooding fixture

This fixture is a stand-in for "a Hephaestus source file" that roadmap item
15 asks a sandboxed Hephaestus lineage to propose and validate an
improvement to. It is deliberately small and boring:
`repository/NOTE.md` has one heading, `# note`, that is inconsistently
lowercase.

`scripts/self_dogfood.sh` runs the whole loop against a disposable copy of
this fixture (never against the real Hephaestus checkout, so the demo can
never mutate this repository's own tracked files):

1. Registers a World and two Genomes: `self-dogfood-parent` (the `identity`
   reference operation — leaves its input untouched) and
   `self-dogfood-candidate` (`ascii_uppercase` — the only other reference
   operation this runtime supports today; see
   `crates/hephaestus-runtime/src/reference_instruction.rs`).
2. Runs the parent, then measures parent versus candidate in the protected
   Arena against `tasks/visible.json` and `tasks/sealed.json`, whose task
   input/expected-output is exactly the fixture heading `# note` /
   `# NOTE`.
3. Computes and replay-verifies the operator-only selection receipt — this
   is real evidence, not a trusted claim from the candidate.
4. Only after that evidence exists does the script (running as ordinary,
   unprivileged shell code outside any sandbox) apply the same, already-Arena-verified
   transform to the fixture copy's actual `NOTE.md` heading and commit it on
   a new local branch, `hephaestus/self-dogfood-proposal`. Nothing is
   merged, pushed, or force-anything; the script prints the branch name and
   stops. A human reviews and merges it (or doesn't) exactly as they would
   any other pull request.

This is intentionally narrow. The only "improvement" the current runtime can
propose is the reference-operation flip; there is no free-form code editing
and no hosted model in this loop (see `AUDIT.md`'s "Remaining product loop").
What this fixture demonstrates and what `docs/SELF_DOGFOODING.md` documents
is the *shape and containment* of the self-improvement loop — evidence before
mutation, a branch instead of a commit to a trunk, and (proven by
`candidate_sandbox_never_receives_merge_or_release_credentials` in
`crates/hephaestus-runtime/tests/adversarial.rs`) no merge or release
credential reachable from the sandboxed steps — not that the proposed
content is a good idea.
