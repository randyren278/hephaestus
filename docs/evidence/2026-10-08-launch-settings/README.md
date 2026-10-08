# Applying source/provider launch settings, 2026-10-08

Actual Claude Code's [fresh-user review](claude-review.txt) identified a concrete
setup trap: a launch with new provider settings silently reused an already-running
daemon. The displayed prompt profiles could not reveal that its executable or
authentication settings had not changed. A comparison could then fail with stale
configuration and retain that failed evaluation identity.

The helper now refuses this implicit reuse when one of its five source/provider
startup variables is present. The fixed message explains how to stop and restart
with those settings, or explicitly attach with `--no-daemon`. Empty and unchanged
settings receive the same refusal; values are not compared or printed. Ordinary
reopening with none of these variables set still works. A fresh start retains
its existing frozen bootstrap and environment handoff.

## Reproduction and check scope

The [native regression](red.txt) fails against the previous helper because it
accepts the ignored setting. After the change, all [three native pilot tests](green.txt)
pass, including existing frozen preparation and matching-evaluator recovery.
The [production-feature tests](default.txt) also pass.

The new native test uses the real helper and daemon, with controlled console
processes in an owned installation fixture. It checks each of the five variables
for terminal and browser launch, refusal before console execution, private-value
non-disclosure, explicit attachment and ordinary reopening. It also exercises an
empty allowlist by itself. The daemon remains frozen, no second daemon/quickstart
is created, and replay passes. This needs neither host Node nor real credentials.
The controlled consoles are not evidence of an actual browser or terminal UI.

The [proof](proof.json) records archive and installed helper/CLI/daemon hashes,
all nine byte-identical copied pack files and owned process cleanup. No provider
launch marker remains. The [previous archive](package-red.txt) fails the installed browser probe: instead
of refusing, its server keeps running until the probe's bounded timeout kills
and waits for the owned helper. The [current archive acceptance](package-acceptance.txt) passes with
real bundled Node and a PTY. The probe starts and restarts with settings, refuses
implicit reuse, opens with explicit attachment, and reopens with settings removed.
The copied guide now gives the exact operator-TUI command for reaching Forge
from the read-only browser workflow. The full harness checks that direct
terminal attachment on the pilot data directory,
terminal restoration, frozen state, absent provider markers and source hashes.

## Guards and minimum compiler

Both new permanent [mutation guards](mutations.txt) are killed on exact Rust 1.88:
bypassing the refusal and refusing explicit attachment. There are no survivor,
stale or timeout results in this two-entry run. The modified sources are restored
byte-for-byte, then [rebuilt and all three native pilot tests rerun](restored-rust188.txt).
The exact [Rust 1.88 workspace all-target/all-feature check](rust188-check.txt) passes.
Both [all-feature](clippy-all.txt) and [default](clippy-default.txt) Clippy pass;
all four [doc tests](doc-tests.txt) pass.

The [fresh workspace run](coverage.txt) passes 724 unit/integration and binary
tests, with no failed or ignored tests. The four separate doc tests bring the
Rust total to 728. All [51 critical-module coverage floors](coverage-gate.txt)
pass at 92% or higher. Optional Rust PTY tests have their environment switch
unset; real terminal behavior is verified separately by installed acceptance.

All [434 configured anchors](anchors.txt) match exactly once. Control shard sizes
and minimums are 29/29/28/28/28. [Actionlint](actionlint.txt) passes. No workflow
actions, permissions, timeout or publication behavior changed. No hosted CI or
fresh full 434-entry result is implied; the separate 432-entry audit is tied to
immutable checkpoint 98ce84d.

Claude's [follow-up](claude-followup.txt) found no blocking implementation issue.
Its documentation observations were addressed: the recognized variables are
explicit, persistent exports require unsetting or explicit attachment, and
allowlisted credential values and PATH also need a restart even though changes
to those values alone are not detected. The empty-only case was added after that
review and is covered by the actual guard/restored run. Codex produced the
execution evidence; Claude only read files.

Profiles describe intended model/authority/budgets, not the daemon's actual
provider login. No authentication, model availability, output quality or revenue
is established by these local fixtures. Native Intel and quarantined-download
acceptance remain open. No credentials or private daemon directories are copied.
