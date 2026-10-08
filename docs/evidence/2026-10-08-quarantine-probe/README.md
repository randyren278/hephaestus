# Local macOS quarantine probe, 2026-10-08

Claude Code's [remaining-work review](claude-remaining-work-review.txt)
identified quarantine handling as a concrete offline delivery check. It found
no verified implementation defect and kept the current full mutation audit
unproven. The probe below tests that hypothesis against the existing native
ARM64 package without calling a provider or changing security settings.

## Extraction and installation

An owned copy of the locally built archive received a synthetic
`com.apple.quarantine` attribute. Its SHA-256 remained
`66d4c06b47e6f6bf3e791b48b40268b8c8dbbc369d6acef2c6a577d6b2365dd5`.
System tar extracted it, and its ordinary installer copied it to a private
prefix. The [extraction proof](quarantine-extraction-proof.json) shows quarantine
on the archive, extracted installer, helper and Node, and installed helper and
Node. Installation exited successfully. This is a synthetic local probe;
no published artifact or browser download is represented.

## Assessment and runtime

On macOS 27.0.1, build 26A434, ARM64, Gatekeeper assessments were enabled.
The [launch proof](quarantine-launch-proof.json) records ad-hoc signatures
without Developer ID identities for all seven Hephaestus binaries. Bundled Node
has its Node.js Foundation Developer ID signature. Gatekeeper's execute
assessment rejects the Hephaestus helper and CLI; its Node rejection says the
code is valid but does not appear to be an app. These assessment results are
recorded separately from actual execution.

The installed CLI and Node version commands both succeed from an agent
subprocess while quarantine remains. That context does not rule out Developer
Tools exemptions or other host policy. It cannot establish a fresh user's
Gatekeeper first launch.

A second extraction and installation of the same quarantined archive passes
the [complete package acceptance script](quarantined-package-acceptance.txt):
relocation, fresh first-launch question, six tour steps, offline Arena/Selection,
replay, bundled terminal and browser consoles, frozen pilot preparation and
attachment, coding evolution, and reference Gauntlet restart/replay.
The script excludes host Node/npm from PATH and requires host Git.

The [post-run proof](proof.json) records terminal exit zero, matching binary
hashes and retained quarantine on all eight packaged binaries after relocation.
It records each checked path and its resolved release path.
The provider marker remains absent and owned app processes are stopped.
No quarantine removal or security-setting change was performed by this probe.

## Delivery consequence

The installation guide now requires release verification before executing a
downloaded archive and links Apple's current
[downloaded-software approval guidance](https://support.apple.com/en-us/102445#openanyway).
Apple describes approval for trusted software that it cannot verify through
Privacy & Security when offered after an attempted launch. This does not
establish that macOS offers that flow for each command-line binary. Messages
reporting detected malware, revoked authorization, or damaged or modified
software require investigation before use.

Claude's [documentation review](claude-quarantine-review.txt) found no blocking
defect or unsafe blanket recovery guidance. Its wording requests were addressed:
the guide distinguishes unverifiable software from detected-malware or damage
messages, the readiness summary scopes assessment rejection to the two tested
Hephaestus binaries, signature claims name the local ARM64 build, and the proof
records the checked paths. Daemon/helper errors are also reportable when no
macOS prompt appears.

Apple Developer ID signing/notarization, an actual published browser download,
and first launch on a fresh user's Mac remain unverified. This probe does not
close those requirements or the full product goal. Codex token reporting was
not expanded: it would not establish the required measured USD cost evidence.
