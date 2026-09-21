# Quickstart fixtures

Everything `scripts/quickstart.sh` registers, in source form.

| File | Registered as | Notes |
|---|---|---|
| `tasks/visible.json` | `arena.visible_manifest` | Candidates may see the input; never the expectation. |
| `tasks/sealed.json` | `arena.sealed_manifest` | Evaluator-only. |
| `world.template.json` | the World | `__VISIBLE_MANIFEST__`, `__SEALED_MANIFEST__`, `__EVALUATOR__`, `__VERIFIER__` are replaced with the addresses the daemon prints for the manifests, the evaluator binary, and `hephaestus verifier`. |
| `parent.json` | root Genome | No parents; the offline `deterministic` / `reference` model. |
| `candidate.template.json` | child Genome | `__PARENT_ID__` becomes the registered parent's content identity, which is why it cannot be a plain file. |

Both Genomes score 0/1 in the Arena. The reference runtime answers every task with a repository inventory, and `expected_output` asks for something else, so the sample proves the measurement pipeline rather than the sample agent.

To adapt the fixtures for your own experiment: keep `schema_version: 1`, change the Laws and ceilings in the World, and point `arena.evaluator` at whichever evaluator binary your daemon was started with. `hephaestus arena evaluate` refuses to run when the World's evaluator hash does not match the executable the daemon was given.
