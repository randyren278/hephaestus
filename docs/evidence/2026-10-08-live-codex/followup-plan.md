# Prepared Forge comparison: policy reference

The owner approved **48 additional calls** with “Approve 48 more calls.”
The first 48-call allowance is exhausted; the separate second comparison
has been submitted once. The registered
[proposal](revision-proposal.json) binds the exact [new prompt](revision.prompt.md)
and hypothesis to the original Selection event and hash. Claude checked this
concrete body [read-only](claude-proposal-check.txt).

The change retains the original first line and replaces the repeated checklist
with a reference to the supplied policy. Queue and priority are considered
independently; no new routing or urgency criteria are introduced. The body
shrinks from 573 bytes / 88 words to 321 bytes / 50 words. These are prompt
lengths, not model token counts or measured provider overhead.

The [read-back profiles](followup-profiles.json) confirm:

- Parent: the evaluated checklist candidate `16b720b7…`.
- Child: the registered Forge revision `54d0d400…`, with that parent only.
- Same World, `codex` / `gpt-6.1-sol`, read-only source authority, network
  permission, strict JSON scoring, 12 visible and 12 sealed tasks.
- Same immutable installed binary digests as the first run. The new
  token-redaction correction is not deployed to this pilot.
- Five-minute / 1 MiB limits per call and 250,000 micro-USD reported-cost
  ceiling. That is a reporting guard, not a provider billing or quota cap.
- Authorized new identity `support-triage-forge-live-2026-10-08-002`:
  **48 additional calls**, 24 per role, using the existing authorized private
  ChatGPT authentication-file handoff. No automatic replacement experiment.

Primary hypothesis: zero paired correctness regressions, using canonical JSON
equality for the whole `queue`/`priority` object. Malformed JSON, extra keys,
refusals and errors do not become excluded successful answers. Any paired
regression refutes the hypothesis. If infrastructure prevents a complete
protected Selection, the outcome is inconclusive and the failed evidence is
retained.

Secondary exploration: total and median signed `latency_millis` over 24 calls
per role. The current harness admits parent trials before candidate trials;
timing drift is confounded with the role. Lower observed latency cannot prove
a repeatable speed effect, override correctness or grant promotion. USD and
saved token counts remain unavailable in these installed binaries.

After this separately authorized run, read back Selection and signed receipts,
record Forge assessment, check the browser export and restart/replay, and
retain a tie or regression. No invariant or Champion claim follows from a
comparison alone. This evidence completes a real Forge comparison leg; it
does not establish private holdout/customer acceptance, measured expenditure,
a release download or an available paid offer.
