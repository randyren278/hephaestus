# Support ticket triage acceptance pack

This pack tests a practical structured-output workflow: route a fictional SaaS
support ticket and assign its priority under an explicit policy. It contains
12 visible cases and 12 sealed cases, including mixed issues, production versus
staging impact, resolved incidents, Unicode and instructions embedded in
customer text. The [policy](policy.md) appears verbatim in every task input.

The two prompts are reasonable initial variants. The baseline applies the
policy directly; the candidate adds a category-precedence, urgency and schema
checklist. The hypothesis is that these checks reduce routing and format
errors without changing the model, policy, task inputs, authority or scoring.
Neither variant is promised to win. Ties and regressions are useful results.

No real-provider run or customer outcome is established by these files. They
are a representative acceptance exercise, not evidence of revenue or broad
generalization. All cases and labels are public and fictional. “Sealed” here
exercises the runtime's evaluator boundary; it is not a confidential benchmark.
Use independently authored private holdouts for a customer pilot.

## Prepare without spending provider quota

Use a fresh daemon data directory and a separate minimal Git repository as
its source repository. Keep this pack, task manifests and expected labels
outside that repository. Start the daemon frozen, with provider setup as
documented in [Runtimes](../../docs/RUNTIMES.md). Registration, profiles and
replay do not call a model. Authenticate with your provider's own tools; do
not put credentials in Genomes, task files or reports.

Choose one provider (`codex` or `claude`) and an explicit model identifier
available to your installed CLI. Use the same identifier for both prompts.
Do not use a moving model alias when you need comparable dated results.

1. Publish [visible tasks](tasks/visible.json), [sealed tasks](tasks/sealed.json),
   the installed reference evaluator and the daemon verifier with the CLI
   commands below. Record their returned artifact identities.
2. Copy [the World template](world.template.json) into your private setup
   directory. Replace its four artifact placeholders. Replace the quoted
   cost placeholder with a non-negative **JSON integer** in micro-US-dollars,
   then register the World. The immutable policy is `json_canonical`.
3. Copy [the baseline template](baseline.template.md), replace the provider
   and model placeholders and register it under that World.
4. Copy [the candidate template](candidate.template.md), replace those same
   model values and the parent placeholder with the baseline's Genome identity.
   Register it under the same World. These are initial variants; this step
   does not claim an evidence-bound Forge proposal.
5. Read both profiles and confirm the complete model, World, authority,
   scoring, task counts and budgets before unfreezing.

```sh
hephaestus --data-dir <fresh-data-dir> arena manifest <pack>/tasks/visible.json
hephaestus --data-dir <fresh-data-dir> arena manifest <pack>/tasks/sealed.json
hephaestus --data-dir <fresh-data-dir> artifact put <installed-reference-evaluator>
hephaestus --data-dir <fresh-data-dir> verifier
hephaestus --data-dir <fresh-data-dir> world register <private-setup>/world.json
hephaestus --data-dir <fresh-data-dir> genome register <private-setup>/baseline.md --world <world-id>
hephaestus --data-dir <fresh-data-dir> genome register <private-setup>/candidate.md --world <world-id>
hephaestus --data-dir <fresh-data-dir> genome profile <baseline-id>
hephaestus --data-dir <fresh-data-dir> genome profile <candidate-id>
```

## Run only within authorized usage

One complete paired comparison makes **48 provider invocations**: both prompts
on the same 24 tasks. Each hosted trial has a five-minute wall deadline and
1 MiB output limit. The aggregate wall limit is four hours plus ten seconds
for protected scoring. The reported aggregate cost allowance is 48 times the
per-trial World ceiling. These are reported receipt limits, not guaranteed
subscription quota or billing caps; Codex reports no USD amount. Review the
profiles and provider usage allowance first.

```sh
hephaestus --data-dir <fresh-data-dir> unfreeze
hephaestus --data-dir <fresh-data-dir> arena evaluate support-triage-v1-001 <baseline-id> <candidate-id>
hephaestus --data-dir <fresh-data-dir> arena select support-triage-v1-001
hephaestus --data-dir <fresh-data-dir> replay
```

Retrying the same evaluation ID reuses its original admission and recorded state; it does not
create an independent repeat. Use a new ID for a deliberately authorized
replication. Inspect the canonical job state before retrying an interrupted
CLI. Freeze or cancel through the console if the run should stop.

## What to accept and record

Acceptance requires real outputs from the selected provider, successful
protected scoring, a canonical Selection bound to this directed pair and
successful replay after restarting the same daemon. Record the exact World,
Genomes, provider CLI digest/version, requested model, date, source commit,
visible and sealed aggregate correctness, paired confidence interval, measured
latency, provider-reported cost or its absence, and all separate gate results.
Use the console's comparison report and verify it against the daemon's
evaluation summary. Keep private holdout text and credentials out of exports.

The evaluator accepts JSON formatting and key-order differences; it rejects
malformed or duplicate-key JSON, extra keys, explanations and incorrect values.
This pack measures strict routing/priority correctness, not answer style or
support quality. A small sample or one successful comparison does not prove
reliable improvement. Preserve unfavorable results and report uncertainty.

After the initial Selection, use `Evidence & Costs → Forge: revise a prompt`
to revise its evaluated candidate from actual evidence. The
[existing checklist prompt body](checklist.prompt.md) belongs to the initial checklist
variant; submitting it unchanged as a revision of that same variant correctly
rejects. Author a distinct evidence-based revision, keep the model and World
fixed, confirm its new comparison and record assessment. Invariant verification
and Champion promotion remain separate operator decisions.
