# Quickstart fixtures

Everything `scripts/quickstart.sh` registers, in source form.

| File | Registered as | Notes |
|---|---|---|
| `tasks/visible.json` | `arena.visible_manifest` | Candidates may see the input; never the expectation. |
| `tasks/sealed.json` | `arena.sealed_manifest` | Evaluator-only. |
| `world.template.json` | the World | `__VISIBLE_MANIFEST__`, `__SEALED_MANIFEST__`, `__EVALUATOR__`, `__VERIFIER__` are replaced with the addresses the daemon prints for the manifests, the evaluator binary, and `hephaestus verifier`. |
| `agent.md` | root Genome | Markdown frontmatter plus the offline `identity` reference instruction. |
| `candidate.md` | child Genome | `__PARENT_ID__` becomes the registered parent's content identity; `ascii_uppercase` transforms Arena task inputs. |

The identity parent returns task input unchanged and scores 0/2. The uppercase child applies its Markdown instruction inside the isolated reference worker and scores 2/2. These are reference-worker operations, not arbitrary natural-language or hosted-model execution.

To adapt the fixtures for your own experiment: keep `schema_version: 1`, change the Laws and ceilings in the World, and point `arena.evaluator` at whichever evaluator binary your daemon was started with. `hephaestus arena evaluate` refuses to run when the World's evaluator hash does not match the executable the daemon was given.
