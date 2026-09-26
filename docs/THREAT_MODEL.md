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

## MCP gateway and remote workers (roadmap item 14)

- The MCP gateway makes its capability-policy decision (per-client `allow`/`grants`) before ever contacting the daemon, but the daemon still enforces every domain invariant (freeze, World law, authority ceilings) on the wrapped command through the ordinary authenticated path — a compromised or misconfigured gateway cannot bypass daemon-side checks, only over- or under-grant which tools it is willing to *attempt*.
- Both an allowed and a denied MCP tool call are ledgered (mcp.call), so a gateway's decisions are auditable even if the gateway process itself is later compromised or replaced; the audit trail does not depend on the gateway's continued honesty.
- A remote worker never holds the daemon's Ed25519 producer key; it returns raw untrusted output that the daemon inspects, signs, and records itself, so a compromised worker can at worst supply an incorrect (never a forged-as-successful) result for the one job it was leased, matching the existing "unreliable result cannot manufacture correctness" control.
- A remote worker credential is content-addressed (`blake3(secret)`) and never persisted in raw form; expiry and revocation are pure functions of replayed history, so they hold across a daemon restart with no extra trust state.
- worker.sock mutual authentication currently relies on the credential (worker → daemon) plus owner-only Unix socket permissions (daemon → worker); this is the same trust model control.sock already uses and does not yet extend to a genuinely cross-host network deployment (no TLS, no daemon certificate pinning) — tracked as a known gap, not a silent assumption.

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

## Verifying it yourself

Tests that pass are not evidence; tests that *fail when they should* are. CI runs the suite, then applies **353 deliberate source mutations**, each one disabling a specific documented invariant (from "an oversized request is accepted" to "a Genome registered before its World is accepted"), and requires the suite to go red for every single one. A mutation that survives fails the build. The mutation jobs run only after the deterministic job passes; inspect the latest CI result before treating a commit as verified. The count can only go up.

Alongside that: an 80% per-module coverage floor (temporarily lowered from 95%; see `TECH_DEBT.md`) on each of 37 production-critical modules (branch coverage where LCOV reports branches, line coverage otherwise), `clippy::pedantic` at deny, `unsafe` forbidden workspace-wide, and a docs gate (`checks/docs_gate.py`) that fails if any path this documentation mentions stops existing.

To reproduce all of it locally: install the stable Rust toolchain with `clippy`, `rustfmt`, and `llvm-tools-preview`, plus `cargo-llvm-cov`. For quick code-quality feedback, run `scripts/check-fast.sh` (or pass a Cargo package name to check one package and its dependencies); it checks formatting and lints all Rust targets, including tests, without linking or launching test executables. Run the full gate before a milestone:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features
PYTHONPATH=python python3 -m unittest discover -s python/tests -v
cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info
python3 checks/coverage_gate.py --manifest checks/checks.json --report lcov.info
python3 checks/mutation_guard.py --manifest checks/checks.json --assert-min 343
python3 checks/docs_gate.py --root . --min-diagrams 1 README.md docs/ARCHITECTURE.md
```

The mutation guard takes a while; `--file-prefix crates/hephaestus-genome/` scopes it to one crate while iterating.
