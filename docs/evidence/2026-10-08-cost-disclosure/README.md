# Recorded costs and pilot delivery, 2026-10-08

The terminal and browser now use the same six-decimal USD formatter, including
one-micro-dollar values. The Runs and Costs panels, comparison cards and report
all explain that these amounts are provider-reported rather than a bill;
hosted zero can be unreported, Codex reports no USD, and totals may omit usage.
Small terminal panels keep a compact disclosure and the selected row.

- [Node 22 TUI tests](tui-tests.txt): 191 pass, including one-micro-dollar values,
  full disclosures, the selected row and panel height at 80 columns, and a
  compact 60×12 terminal slot. [Typecheck and packaged bundle](tui-build.txt) pass.
- [Web typecheck, tests and build](web-checks.txt): 40 pass. Both the card and
  exported report retain the shared formatting and disclosure, including when
  there is no Selection yet.
- [Live 80×24 terminal](terminal-evidence.txt) reads real Runs, Evidence, Costs
  and Denials from the installed daemon. Runs and Costs retain all three
  disclosure lines and terminal settings are restored on exit. The refused
  request used a blank request ID against this owned scratch daemon.
- [Fresh archive acceptance](package-acceptance.txt) passes isolated installation
  and relocation, first-launch choice, all six tour steps, offline Arena,
  coding and reference Gauntlets, replay and terminal restoration. Host
  Node/npm are hidden; the archive supplies Node 24.21.0.
  [Package proof](package-browser-proof.json) records its SHA-256 and confirms
  the tested TUI bundle equals the current source build. The nine
  [package-builder contracts](package-contracts.txt) pass.
- The actual browser **Download evidence report** button produced this
  [report](comparison-report.md). Its identifiers, visible scores, both
  recorded costs and latencies match the saved
  [daemon readback](evaluation-summaries.json), as the
  [report check](report-check.txt) confirms. The full disclosure is visible in
  the [desktop](desktop-comparison.png) and [390-pixel mobile](mobile-comparison.png)
  comparisons. Mobile document width is 375 pixels, within the 390-pixel
  viewport; both costs are readable without page overflow.
- [Pilot setup](pilot-setup-check.txt) proves the documented directory is
  private and durable. Global `commit.gpgsign=true` with an unusable signer
  rejects the unmodified commit; the documented local override succeeds.
- Actual Claude Code reviewed the source and fresh-reader instructions. Its
  [initial](claude-initial.txt), [follow-up](claude-followup.txt) and
  [readiness review](claude-readiness-review.txt) prompted the compact layout,
  partial-total disclosure, precision assertions, durable setup directory,
  installer minimum and explicit report controls. Later evidence above
  addresses the review's acceptance-scope questions.
- Claude's [final checkpoint review](claude-final-checkpoint.txt) caught the
  installed evaluator symlink in the pilot daemon command. The corrected
  command resolves the real file with Node, preserving the evaluator's
  no-symlink guard. [Both install modes](evaluator-resolution.txt) resolve to
  their identical regular executables. The exact package resolution command
  then passes [real offline Arena, Selection and replay](evaluator-path-acceptance.json).
  This is a reference-fixture path check, not a hosted support-triage run.

All task outputs here come from offline reference fixtures. These records do
not establish hosted-model quality, provider billing, a paid plan, native Intel
acceptance, hosted release delivery or downloaded/quarantined first launch.
The web console currently runs from a source checkout; the native archive
contains the terminal console. The [pilot guide](../../PILOT_GUIDE.md) states
this delivery and commercial scope.
