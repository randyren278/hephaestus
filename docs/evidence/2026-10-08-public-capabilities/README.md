# Public capability descriptions, 2026-10-08

The public status page still said Forge could not record a prose prompt
revision. The implementation and the real
[Forge comparison](../2026-10-08-live-forge/README.md) contradict that statement.
The status, Genome, evolution and runtime guides now distinguish
operator-written hosted revisions from automatic reference-operation changes.
The readiness record also links the verified passing CI checkpoint and retains
the exhausted experimental allowances and remaining full-product requirements.

[Actual Claude Code review](claude-review.txt) inspected the code and saved
evidence with read-only tools. Codex checked its findings against
the revision implementation, CLI command, terminal menu and existing live
comparison evidence. Claude did not run tests or independently inspect GitHub;
the supplied CI result was separately read back by Codex.

[CI snapshot](ci-checkpoint.json) records the completed successful run at
`739cb9cb71acd0a83f980bdc21652ea6006399c7`, with all 29 jobs successful.
This is the pre-documentation checkpoint, not a CI result for these edits.
[Documentation checks](docs-check.txt) validate relative links, diagrams and
repository paths. No runtime code, tests, manifest entries or quality gates
changed. The checks do not prove new model quality or customer acceptance.

A separate nonpublishing release rehearsal was dispatched at the same source
checkpoint:
[run 37860441104](https://github.com/randyren278/hephaestus/actions/runs/37860441104).
It exercises native ARM64/Intel acceptance, dependency inventories and
same-machine archive reproducibility. The workflow's signing and publishing
jobs require a tag-push event and cannot run for this manual dispatch. Dispatch
and in-progress jobs are not counted as acceptance; its eventual result must
be inspected separately.
