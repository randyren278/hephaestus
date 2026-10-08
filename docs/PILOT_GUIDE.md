# Evaluate one prompt change

Use Hephaestus when your team changes an AI prompt for a repeatable workflow
and needs a decision it can inspect later. Ticket routing, structured extraction
and classification are useful starting points because their outputs can be
checked against explicit expected values. The first pilot should answer one
question: did this particular change help on these tasks, under these rules?

This guide is for an engineer who owns the workflow, its expected answers and
the provider account. Start with one workflow, one provider/model and one
prompt revision. The [support-triage pack](../examples/support-triage/README.md)
is a public fictional example you can prepare without calling a model.

## What you get

A completed pilot produces:

- An immutable task/scoring configuration and two exact prompt identities.
- A paired comparison on the same visible and sealed tasks, with correctness,
  uncertainty, measured latency and recorded cost where the provider supplies it.
- A canonical Selection receipt and a readable comparison report.
- A restart and replay check showing that the saved evidence remains consistent.
- A decision to keep, revise or reject the change, with the remaining gates shown.

The report summarizes evidence; it is not itself a signed receipt. A positive
comparison does not automatically promote an agent. Invariant verification,
Forge assessment and a Champion transition remain separate decisions.

## Prepare the pilot

1. **Choose a narrow workflow.** Use inputs you can lawfully give to your model
   provider and outputs with checkable answers. Write down how errors affect
   the workflow and what counts as success before running anything.
2. **Freeze the test set and scoring.** Separate development examples from an
   independently authored private holdout. Keep both expected labels and the
   evaluator outside the candidate source repository. Public fixture labels
   are useful for setup, not a confidential benchmark.
3. **Choose a fair pair.** Keep the provider, explicit model, task inputs and
   authority fixed. Start from a reasonable current prompt. Record one
   specific change and hypothesis rather than weakening the baseline.
4. **Set the usage allowance.** Review task counts, per-trial deadlines,
   aggregate work, output limits and the World cost ceiling in both profiles.
   Approve provider usage through your normal account process before running.
5. **Install and prepare.** Follow [Getting started](GETTING_STARTED.md) for
   source installation or [macOS installation](MACOS_INSTALL.md) for package
   requirements. Complete the deterministic first-launch tour, then follow
   the acceptance pack's registration instructions with a fresh data directory
   and separate source repository. The daemon starts frozen.

The installed CLI copies the public pack and its local provider guide with
`hephaestus init --fixture support-triage /new/pilot/directory`. Follow its copied
README to start the matching installation's frozen daemon, then use
`hephaestus --data-dir /pilot/data pilot prepare /pilot --provider codex --model EXACT_MODEL_ID --cost-microusd 250000`.
Choose your own provider, available exact model and positive reported cost ceiling.
Preparation registers the pair and reads profiles; it does not authenticate or
call a model. Package users need no source checkout or host Node/npm/Python for
these steps. Actual hosted work still requires your selected provider CLI and
explicit account handoff.

Clean source and locally built ARM64 package checks pass. The
[nonpublishing release rehearsal](evidence/2026-10-08-native-release-rehearsal/README.md)
also passes installed-package acceptance on native Apple Silicon and Intel
macOS runners. Hosted release signing/publication and first launch from a
quarantined browser download remain unverified. Follow
[product readiness](PRODUCT_READINESS.md) before choosing a delivery route.

## Measure and decide

Run the paired comparison and record Selection. Start the
[installed web console](../apps/hephaestus-web/README.md#launch-from-an-installation)
with your pilot data directory. Open **Evidence & activity**, then the
comparison's **Agent identities and report** details and choose
**Download evidence report**. Check its scores and gate results against the daemon.
Use the printed pilot directory in the console command:

```sh
heph web --data-dir "/your/printed/pilot/directory/data" --no-daemon
```

Restart the same daemon and replay the saved history. Retain a tie or regression
as carefully as a win. Use new evaluation identities only for deliberately
authorized independent repeats; retrying an old identity reuses its admission.

Check the confidence interval and sample size before interpreting improvement.
Review visible and sealed aggregate correctness separately. An improved average
can still include unacceptable regressions. Latency measures this execution;
it is not a service-level promise.

Cost figures are provider-reported USD rather than a billing statement. Codex
reports no USD amount; a hosted zero can mean missing reporting. World cost
and wall limits do not guarantee subscription quota or provider billing caps.
Use your provider's usage statement to understand the actual expenditure.

The [first real support-triage comparison](evidence/2026-10-08-live-codex/README.md)
completed 48 Codex calls: both prompts scored 24/24 and the checklist did not
pass the measured gates. The record includes the checked browser report and
restart/replay evidence. It establishes this workflow on a public fictional
pack; it does not establish an improvement, measured USD cost or customer value.
The [evidence-bound Forge follow-up](evidence/2026-10-08-live-forge/README.md)
completed 48 more calls. It also tied 24/24, and Forge recorded
`metrics_rejected`; no revision was promoted. Both authorized experiments
are complete, with 96 successful experimental calls in total.

If the evidence identifies a useful next change, use the guided Forge revision
flow to bind a new prompt and hypothesis to the evaluated candidate's source
Selection. Confirm its separate comparison and record assessment. Keep the
World, model and scoring fixed so the new result answers the same question.

## Delivery and support

Hephaestus is MIT-licensed software you run locally with your own provider
account. The offline tour uses no paid model. Hosted comparisons consume your
provider quota or API billing. Provider login and availability remain your
account's responsibility; the tool does not supply a model subscription.

For a bug, use the repository's [issue tracker](https://github.com/randyren278/hephaestus/issues)
with your platform, software versions, command, terminal result and a small
fictional reproduction. Keep credentials, real customer tickets and private
holdout content out of public issues. No response-time guarantee is offered.

A paid assisted pilot would be a separately agreed service: help choose one
task/scoring contract, install locally, run an authorized comparison and deliver
the checked evidence package and decision. Agree on the fee, provider allowance,
data handling, support and acceptance criteria before work begins. This is a
proposed commercial scope, not an available paid plan or a promise that a prompt
will improve. A hosted service and payment flow have not been established.
