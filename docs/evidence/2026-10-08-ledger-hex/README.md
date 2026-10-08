# Strict ledger hex decoding, 2026-10-08

Claude identified a malformed-record edge case: the decoder checked the UTF-8
byte length, then consumed pairs of characters. A trailing `é` or `🛠` has an
even byte length but only one character, so the old decoder discarded it and
accepted the unchanged decoded event. This did not forge a signed event, but
it failed to reject malformed complete record fields.

The decoder now requires ASCII after its existing even-byte-length check.
Payload, predecessor hash and event hash all use this decoder. Valid payloads
can still contain any binary or UTF-8 bytes; their hexadecimal representation
is ASCII. Uppercase hex remains accepted.

- The [first reproduction](regression-before.txt) failed before the guard.
  It used an earlier formatting draft with only the rejection test added.
  After the compatibility test and formatting were final, the
  [exact current rejection test](regression-current-before.txt) also failed
  with only the ASCII guard removed. The isolated source was restored byte
  for byte and its [ledger tests passed again](restored-ledger-tests.txt).
- [All 20 ledger tests](ledger-tests.txt) pass: 9 unit tests, 4 durable-spine
  tests and 7 JSONL durability tests. The regression covers both Unicode
  suffixes in all three fields, requires `MalformedRecord` and verifies that
  rejected complete file bytes remain untouched. The compatibility test
  replays a Unicode payload and uppercase hex as the exact original event.
- [Rust 1.88](rust-1.88-check.txt) checks all workspace targets and features
  with both Cargo and rustc pinned. [Ledger Clippy](clippy.txt) passes with
  warnings denied. Exact commands and source hashes are in the [proof](proof.json).
- [All 15 ledger mutations](mutations.txt) passed their mandatory baseline
  and were killed, with no survivors, stale entries or timeouts. One new
  mutation removes the ASCII guard; the current exact regression kills it.
  Sources were restored and compared byte for byte with the working tree.
  The CI ledger floor rises to 15, matching the actual scope. The configured
  full matrix is now 429 entries; the earlier complete 428-entry result is a
  separate historical checkpoint, not a fresh 429-entry run.
- The refreshed [all-feature workspace run](workspace-tests.txt) passes
  717 tests including doc tests, with zero failures or ignored tests. Both
  [all-feature](clippy-all.txt) and [default-feature](clippy-default.txt) workspace
  Clippy modes also pass with warnings denied.
- The rebuilt native ARM64 archive passes [installed acceptance](package-acceptance.txt)
  again: isolated home, relocation, first-launch choice, six-step measured tour,
  offline Arena, both Gauntlets, replay and restored terminal. Host Node/npm
  are hidden. The proof records the new archive hash and confirms its TUI
  bundle equals the current build. This is a local unsigned archive.
- [Actual Claude Code review](claude-review.txt) found no introduced defect
  or material missing regression coverage. It also confirmed the prior pilot
  evaluator-symlink correction. No hosted-provider result is inferred.
