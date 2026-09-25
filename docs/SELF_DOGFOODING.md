# Self-dogfooding

Roadmap item 15 asks for "a documented, runnable procedure + fixture where a
sandboxed Hephaestus lineage proposes and validates an improvement to a
Hephaestus source file... and the result is a branch/patch that a human must
merge — prove no merge/release credentials are reachable from the candidate
sandbox (test)."

## Run it

```sh
scripts/self_dogfood.sh
```

This builds the workspace, copies `examples/self_dogfood/repository` into a
disposable scratch directory (never the real Hephaestus checkout — the demo
cannot mutate this repository's own tracked files), and then:

1. Registers a World and two Genomes against that scratch repository: an
   `identity` parent and an `ascii_uppercase` candidate.
2. Runs the parent, then measures parent versus candidate in the protected
   Arena against a task whose input/expected-output is exactly the fixture's
   heading line, `# note` / `# NOTE`.
3. Computes and replay-verifies the operator-only selection receipt. This is
   real ledger evidence — hash-linked, replay-checked, produced by the
   daemon's own verified history — not a claim taken on the candidate's word.
4. Only after that evidence exists, applies the same already-verified
   transform to the fixture's actual `examples/self_dogfood/repository/NOTE.md`
   (its scratch-directory copy) and commits it on a new local branch,
   hephaestus/self-dogfood-proposal, in the scratch repository. Nothing is
   pushed or merged; the script prints the branch and a `git diff` command so
   a human can review it.

See `examples/self_dogfood/README.md` for the fixture layout.

## What this proves

- **Evidence gates the file mutation.** Step 4 only runs after the Arena has
  measured and the daemon has replay-verified that the candidate's transform
  actually improves on the parent for the given task — the script does not
  trust unverified candidate output.
- **The result is a branch, not a commit to a trunk.**
  `scripts/self_dogfood.sh` never checks out or fast-forwards `main`; it
  stops on a side branch and a human decides whether to merge it, exactly
  like reviewing a pull request.
- **No merge or release credential is reachable from the candidate sandbox.**
  This is the specific, tested claim: `IsolatedWorker::execute` (the code
  path every candidate and evaluator process in this pipeline runs through)
  calls `Command::env_clear()` and then sets only `PATH`, `HOME`, and
  `TMPDIR` — see `crates/hephaestus-runtime/src/worker.rs`. The test
  `candidate_sandbox_never_receives_merge_or_release_credentials` in
  `crates/hephaestus-runtime/tests/adversarial.rs` poisons a child process's
  ambient environment with every credential shape a merge/push/release step
  could need (`GIT_ASKPASS`, `GIT_SSH_COMMAND`, `GITHUB_TOKEN`, `GH_TOKEN`,
  `SSH_AUTH_SOCK`, `NPM_TOKEN`, `CARGO_REGISTRY_TOKEN`, `COSIGN_PASSWORD`,
  `SIGSTORE_ID_TOKEN`) and proves none of it reaches a real `IsolatedWorker`.
  It was verified locally to actually catch a regression (temporarily
  removing the `env_clear()` call made the test fail), so it is not a
  tautology. Separately, the branch-materialization step (item 4 above) runs
  as ordinary unprivileged shell code *after* the sandboxed evaluation is
  done, never inside a candidate or evaluator sandbox, and the scratch
  repository never has a `git remote` configured — the script asserts this
  both before and after the run.

## What this does not prove

- **This is not free-form code editing by a model.** The only mutation the
  runtime supports today is the reference-operation flip
  (`identity` → `ascii_uppercase`); there is no hosted-provider adapter and
  no LLM in this loop (see `AUDIT.md`'s "Remaining product loop"). The
  "improvement" here is a fixed, deterministic transform already known to
  the system, applied to a fixture chosen to make that transform look like a
  plausible doc fix. It stands in for a real code-editing capability that
  does not exist yet.
- **It targets a disposable fixture copy, not this repository.** A stronger
  future version would run the loop against a real Hephaestus source file in
  a genuinely isolated clone and open an actual pull request; that is not
  wired up. Tech debt.
- **It is not evolution.** `hephaestus evolve` (the daemon's unattended,
  budget-bounded generational loop) is a separate, already-documented
  capability (`AUDIT.md`); this script drives the same primitives (`run`,
  `arena evaluate`, `arena select`) by hand, once, for one fixed pair of
  Genomes.
- **No CI job runs `scripts/self_dogfood.sh`.** Like `scripts/quickstart.sh`, it is a
  macOS-only, Seatbelt-dependent, developer-run demonstration; only the
  credential-containment property has an automated, CI-covered test
  (`candidate_sandbox_never_receives_merge_or_release_credentials`, which is
  cross-platform because it does not need a live sandbox).
