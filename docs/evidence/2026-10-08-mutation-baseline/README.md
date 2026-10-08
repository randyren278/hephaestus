# Native mutation baseline deadline, 2026-10-08

The isolated native macOS source run's full uninstrumented Rust suite passed
715 tests with no failures; its test execution took about 181 seconds. The
mutation guard then stopped before any mutation because its baseline deadline
was 120 seconds. Both the [successful Rust baseline](rust-baseline.txt) and
[original timeout](original-baseline-timeout.txt) are retained.

The guard now accepts a separate baseline-only deadline. Its default stays
unchanged. Extending that prerequisite does not skip it or alter any mutation's
120/240/480-second manifest deadline. Invalid explicit or inherited baseline
values reject before running a suite. A failing baseline still rejects before
changing source. Mutation timeouts remain failures rather than killed verdicts.

- [Python checks](python-tests.txt): 47 passed. Real subprocess cases prove
  that a baseline may take longer than the mutation deadline, while a stalled
  mutant still times out and source is restored. The exact observed deadline
  arguments are checked. A failed baseline never admits mutation work.
- [Claude review](claude-review.txt): no concrete blocker. Its 100-ms test
  timing concern was addressed with a one-second mutation deadline and
  five-second baseline margin before the final Python run. Non-numeric and
  boolean inherited deadlines now reject with a diagnostic.
- Two new mutation entries guard deadline independence and the mandatory green
  baseline. The checks shard floor increased from three to five; the full
  matrix increased from 426 to 428 entries.

- [Affected mutations](mutation-results.txt): all five killed, none survived,
  stale or timed out. The [restoration proof](restoration-proof.json) verifies
  the isolated checkout's three copied source files match the final source
  bytes after the run and that Rust crates were untouched.

The complete matrix remains a separate product requirement; these scoped checks cannot
substitute for it. The intended native invocation is:

```sh
python3 checks/mutation_guard.py --manifest checks/checks.json --baseline-timeout 900 --assert-min 428
```

Run mutations only in an isolated checkout. Source restoration and a rebuild
from restored source must pass before using its binaries for product evidence.
