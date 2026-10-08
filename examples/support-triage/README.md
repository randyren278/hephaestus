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

## Create an installed pilot

The macOS package includes this pack. Source installations also use the same
initializer. Git is required; package users need no host Node, npm, Python or
source checkout for these steps.

```sh
mkdir -p "$HOME/.local/share/hephaestus/pilots"
PILOT_ROOT=$(mktemp -d "$HOME/.local/share/hephaestus/pilots/pilot.XXXXXX")
PILOT_DIR="$PILOT_ROOT/support-triage"
hephaestus init --fixture support-triage "$PILOT_DIR"
printf 'Pilot directory: %s\n' "$PILOT_DIR"
HEPHAESTUS_SOURCE_REPOSITORY="$PILOT_DIR/repository" \
  heph web --data-dir "$PILOT_DIR/data"
```

The initializer copies this pack and creates a committed minimal Git repository
under its repository subdirectory. The task manifests, expected labels and
prompts stay outside that source repository. It preserves an existing destination
by refusing to overwrite it. The random parent directory is private to its owner.
`heph web` starts the daemon frozen, resolves the installed protected evaluator
without a host Node command, and opens the read-only browser server. Open its
printed URL; leave the terminal running. It does not run an agent.

## Prepare without spending provider quota

In another terminal, set `PILOT_DIR` to the directory printed above. Choose one
provider (`codex` or `claude`) and the exact model identifier you intend to use.
The preparation command does not check model availability or authenticate a
provider. An alias may change over time; use a dated identifier when available.

```sh
PILOT_DIR="/your/printed/pilot/directory"
hephaestus --data-dir "$PILOT_DIR/data" pilot prepare "$PILOT_DIR" \
  --provider codex --model EXACT_MODEL_ID --cost-microusd 250000
```

All three flags are required. The cost must be 1 through 1,000,000,000 micro-USD;
invalid limits are rejected before registration. The cost example is a $0.25 **reported per-trial**
ceiling: 48 invocations would have a $12 reported aggregate allowance. Choose
it from your expected usage; it is not a billing or subscription quota cap.
Codex reports no USD amount, so recorded cost gates cannot prove actual spending.

Preparation requires a frozen daemon with no active runs. Use the CLI and
`heph` from the same installation, so the CLI-adjacent reference evaluator
matches the daemon. A custom daemon evaluator is not supported by this helper;
evaluator identity is checked again before any comparison trials start. If it
does not match, use the [matching-installation recovery steps](provider-setup.md#recover-an-evaluator-installation-mismatch);
repeating preparation through a mismatched CLI does not fix the binding. It publishes both
manifests, the installed evaluator and daemon verifier, replaces the World and
prompt placeholders, registers the World and directed pair, and reads back both
profiles. It never unfreezes or submits provider work. Its output includes the
exact World, parent and candidate identities, both complete profiles, and the
private directory containing generated setup files. With the CLI's `--json`
flag, stdout contains one JSON object. Original pack files are preserved.

Registration uses ordinary daemon commands and is not an atomic transaction.
If interrupted, retain the data directory and retry with the same inputs:
immutable registrations reuse their identities. Changed input files or model/cost
flags deliberately create new identities. Confirm the resulting profiles again.
Every attempt that passes the frozen check retains a new private setup directory;
its path is printed even if a later registration fails.
A successful preparation is configuration evidence, not a completed evaluation.

Read both profiles and confirm the full provider/model, same World, read-only
source authority, network permission, strict JSON scoring, 12 visible plus 12
sealed tasks, and budgets. Then follow the copied [provider setup guide](provider-setup.md)
to restart this daemon with your explicitly authorized authentication handoff.
Your normal host login is not automatically available in the provider's private
HOME. Never put credentials in Genomes, task files or reports.

For manual registration, the [World template](world.template.json),
[baseline](baseline.template.md), [candidate](candidate.template.md),
[visible manifest](tasks/visible.json) and [sealed manifest](tasks/sealed.json)
remain available. The World cost placeholder must become a JSON integer.
Model identifiers belong in quoted YAML scalars. The immutable scoring policy
is `json_canonical`. These are initial variants, not an evidence-bound Forge proposal.

## Run only within authorized usage

One complete paired comparison makes **48 provider invocations**: both prompts
on the same 24 tasks. Each hosted trial has a five-minute wall deadline and
1 MiB output limit. The aggregate wall limit is four hours plus ten seconds
for protected scoring. The reported aggregate cost allowance is 48 times the
per-trial World ceiling. These are reported receipt limits, not guaranteed
subscription quota or billing caps; Codex reports no USD amount. Review the
profiles and provider usage allowance first.

Use the exact `parent_genome_id` and `candidate_genome_id` from preparation:

```sh
PARENT_ID="your printed parent_genome_id"
CANDIDATE_ID="your printed candidate_genome_id"
hephaestus --data-dir "$PILOT_DIR/data" unfreeze
hephaestus --data-dir "$PILOT_DIR/data" arena evaluate support-triage-v1-001 "$PARENT_ID" "$CANDIDATE_ID"
hephaestus --data-dir "$PILOT_DIR/data" arena select support-triage-v1-001
hephaestus --data-dir "$PILOT_DIR/data" replay
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

After the initial Selection, open the operator terminal console in another terminal:

```sh
hephaestus --data-dir "$PILOT_DIR/data" tui
```

Choose `Evidence & Costs → Forge: revise a prompt` to revise the evaluated
candidate from actual evidence. The browser console is read-only. The
[existing checklist prompt body](checklist.prompt.md) belongs to the initial checklist
variant; submitting it unchanged as a revision of that same variant correctly
rejects. Author a distinct evidence-based revision, keep the model and World
fixed, confirm its new comparison and record assessment. Invariant verification
and Champion promotion remain separate operator decisions.
