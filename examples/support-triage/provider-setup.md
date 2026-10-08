# Provider setup for the pilot

Preparation and profile readback do not call a model. The model label is a
requested identifier, not proof that the provider supports it. Actual evaluation
requires your selected CLI, your own authorized account handoff and enough quota
for both prompts on all 24 tasks: 48 invocations. Review usage before unfreezing.

Each invocation has its own private HOME. Hephaestus does not inherit a normal
host login automatically and does not manage your provider login. Run the
provider's login process yourself. Keep credentials outside the source repository,
task manifests and exported evidence.

## Restart the same frozen daemon

Provider executable and authentication configuration are captured when the daemon
starts. Stop the browser with Ctrl+C, then stop the daemon serving this pilot:

```sh
heph stop --data-dir "$PILOT_DIR/data"
```

Choose the matching configuration below and launch the browser again. Starting
the same data directory replays the registered World and pair and stays frozen.
Use absolute provider executable paths. The new browser launch prints a new URL.

For **Codex with a ChatGPT login**, authenticate with your own Codex CLI first.
Its owner-only auth file can be handed to the daemon explicitly:

```sh
HEPHAESTUS_SOURCE_REPOSITORY="$PILOT_DIR/repository" \
HEPHAESTUS_CODEX_EXECUTABLE="/absolute/path/to/codex" \
HEPHAESTUS_CODEX_AUTH_FILE="$HOME/.codex/auth.json" \
  heph web --data-dir "$PILOT_DIR/data"
```

The daemon copies that one file into each admitted Codex run's private HOME;
it does not expose your whole Codex directory. This command does not run a model.

For **Claude Code with a subscription**, use your CLI's `claude setup-token`
process yourself. Keep the resulting token in an owner-only file and load it
into `CLAUDE_CODE_OAUTH_TOKEN` through your normal secure account setup. Do not
paste it into a report, source file or command argument. With that environment
variable already set, launch:

```sh
HEPHAESTUS_SOURCE_REPOSITORY="$PILOT_DIR/repository" \
HEPHAESTUS_CLAUDE_EXECUTABLE="/absolute/path/to/claude" \
HEPHAESTUS_PROVIDER_ENV_ALLOWLIST=CLAUDE_CODE_OAUTH_TOKEN \
  heph web --data-dir "$PILOT_DIR/data"
```

For **API authentication**, set the appropriate key securely in the daemon's
environment and explicitly name that variable in
`HEPHAESTUS_PROVIDER_ENV_ALLOWLIST`. Only listed variables reach the provider
child. This can incur API charges; a World cost ceiling checks reported usage,
not the provider's bill.

## Confirm before evaluating

Read both recorded profiles again. Keep provider, exact model, World, source
commit, task manifests and authority fixed for the directed pair. The daemon
starts frozen; evaluation begins only after an explicit unfreeze and comparison
command. The copied [pack guide](README.md#run-only-within-authorized-usage)
describes that step, its limits and the required evidence.

Subscription usage and API billing are external to Hephaestus. Codex reports no
USD amount; hosted zero can mean absent reporting. A successful preparation
does not prove authentication, requested-model availability or hosted quality.

## Recover an evaluator installation mismatch

If a comparison says the configured evaluator does not match the World's pinned
evaluator, no new comparison was admitted. Stop the browser with Ctrl+C and
stop its daemon with `heph stop --data-dir "$PILOT_DIR/data"`. Restart that same
data directory using the installation whose evaluator was used to prepare the
World, with the same source and provider configuration. Then read both profiles
and retry. A version label alone does not prove the executable bytes match.

Re-running preparation through the same mismatched CLI can return the same World
and repeat the refusal. Keep CLI, `heph` and daemon from the matching installation.
The helper does not support a custom daemon evaluator. An existing World is
immutable; changing the daemon executable does not change its pinned evaluator.

A missing or unreadable evaluator can instead be restored at its configured path
from the matching installation, then retried without restarting. The evaluator
must be a regular executable file with a single hard link and no symlink. These
checks remain mandatory; restoring another binary with the same filename does
not satisfy the digest binding. Retain prior data and evidence during recovery.
