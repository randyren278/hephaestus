# Threat Model

## Assets

Hephaestus protects canonical ledger history, Genome and World identity, sealed evaluators, artifact integrity, operator authority, credentials, budgets, promotion state, rollback state, and the confidentiality of candidate siblings.

## Trust boundaries

- The operator boundary authenticates people and issues short-lived authority proofs.
- The daemon is the sole canonical writer.
- Candidate runtimes and their outputs are untrusted.
- Repositories, tool results, retrieved experience, model output, and remote workers are untrusted inputs.
- Evaluators and sealed holdouts are isolated from candidates.
- The artifact store is verified by content hash on every read.

## STRIDE analysis

| Category | Concrete threat | Required control |
|---|---|---|
| Spoofing | A candidate labels itself as the operator and clears freeze | Operator actions require an authority-root-issued proof; caller-supplied role labels are insufficient |
| Tampering | A candidate edits a World, evaluator, event, or artifact to improve its score | Deny candidate write access; hash-link events; content-address artifacts; verify hashes before use |
| Repudiation | A worker denies a tool call, denial, cost, or promotion recommendation | Ledger actor, capability, input/output hashes, parent event, and decision receipt |
| Spoofing | A raw ledger writer fabricates a successful `runtime-plane` result | Daemon-held Ed25519 producer key; World-anchored public verifier; signed full envelope and claims |
| Information disclosure | A candidate reads sealed tasks, sibling workspaces, credentials, or raw secrets in traces | Separate sandboxes and evaluator identities; no ambient credentials; redact before persistence |
| Denial of service | Unbounded input, recursion, retries, processes, tokens, or disk exhausts the daemon | Bound request size, concurrency, time, tokens, cost, output, and storage; kill descendants on expiry |
| Elevation of privilege | A child widens capabilities or reuses an expired token | Parent-subset derivation, scoped opaque tokens, expiry, audience binding, and deny-by-default checks |

## Current implementation review

Capability subset checks are fail-closed, and operator control uses a non-loggable opaque token matched against canonical state. The owner-only daemon transport has bounded requests and expiring run capabilities. Runtime result claims are signed after completion with a separate daemon-only Ed25519 seed; replay and Arena require the anchored verifier, so a forged actor string or raw append is insufficient. Candidate and evaluator helpers have distinct offline worker domains, owner-only request roots, bounded IPC, process-group cleanup, and protected-path policies. The trusted scheduler launches complete World-bound parent/candidate task sets through separate candidate worker roots before Arena re-hashes and launches the World-bound evaluator. Arena accepts only a canonical aggregate response committed to the exact evaluator request. An unreliable result cannot manufacture correctness by emitting the expected bytes: unsuccessful terminal results are forced incorrect while their authenticated cost and latency remain evidence. Candidate-visible results omit the operator event hash and all sealed fields. The separate `OperatorEvaluation` capability binds aggregate selection evidence to authenticated run-event hashes and the exact verified evaluation-event hash; replay verifies ledger and CAS state before that capability can be reconstructed after restart. Experience rehydration likewise validates canonical receipts and artifacts, earlier distinct source events, evidence CAS objects, exact provenance, and the final event hash, but those Experience receipts are not separately signed. It therefore proves store consistency rather than producer identity or redaction against a fully self-consistent raw-store rewrite and must not serve as authorization evidence. Current controls protect against candidate and ordinary raw-ledger forgery, not a compromised daemon, stolen key, malicious operator, same-UID/root filesystem access, a fully recomputed data directory, or wholesale rollback without an external checkpoint.

## Supply chain and release

| Category | Concrete threat | Required control |
|---|---|---|
| Tampering | A compromised or malicious dependency is pulled into a release build | `cargo deny check` (licenses, RustSec advisories, crates.io-only sources) and `npm audit --audit-level=high` run in CI for every push (`.github/workflows/ci.yml`) |
| Tampering | A CI Action is repointed to malicious code by moving its version tag | Every third-party Action in CI and release workflows is pinned to a commit SHA, not a tag |
| Repudiation | A published archive cannot be tied back to the exact source and build that produced it | Keyless Sigstore signatures, GitHub build-provenance attestations, and a same-machine reproducibility check on every tagged release (see `docs/RELEASES.md`) |
| Elevation of privilege | A workflow job is granted more GitHub permissions than its steps need | `permissions: contents: read` at the workflow root; only the jobs that publish or sign a release elevate to `contents: write` / `id-token: write` / `attestations: write` |

`docs/RELEASES.md` states plainly what the reproducibility check does and does not prove: same-machine determinism, not an independent third-party rebuild.

## Self-dogfooding

Roadmap item 15 requires proof that no merge or release credential is reachable from a sandboxed candidate lineage before Hephaestus is trusted to propose changes to its own source. This is a narrower, testable instance of the "information disclosure" row above (`A candidate reads sealed tasks, sibling workspaces, credentials, or raw secrets in traces`): `IsolatedWorker::execute` (`crates/hephaestus-runtime/src/worker.rs`) clears the child process environment and sets only `PATH`, `HOME`, and `TMPDIR`, so `GIT_*`, `SSH_AUTH_SOCK`, `GITHUB_TOKEN`/`GH_TOKEN`, package-registry tokens, and signing secrets are never inherited. `candidate_sandbox_never_receives_merge_or_release_credentials` in `crates/hephaestus-runtime/tests/adversarial.rs` exercises this directly. See `docs/SELF_DOGFOODING.md` for the full runnable procedure and its honest scope.

## Security acceptance

Every critical control path must have positive, negative, and mutation tests. `docs/ADVERSARIAL.md` names the sandbox escape, evaluator leakage, budget bypass, event tamper, corruption, and partial-promotion tests that satisfy this for roadmap item 15; stale-token and sibling-isolation fault tests remain open work.
