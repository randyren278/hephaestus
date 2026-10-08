# Full mutation matrix, 2026-10-08

The complete configured matrix ran against detached commit `709d965` after
its unmodified all-feature workspace baseline passed:

**428 run; 428 killed; 0 survived; 0 stale; 0 timed out.**

The [results](mutation-results.txt) preserve each command and verdict.
The [proof](proof.json) names the immutable source commit and scope.
After the guard restored its files, [all-feature workspace binaries rebuilt](restored-build.txt)
and `git diff --exit-code` returned zero. Each mutation kept its configured
120-, 240- or 480-second deadline; only the full-suite baseline used 900 seconds.

```sh
python3 -u checks/mutation_guard.py --manifest checks/checks.json --baseline-timeout 900 --assert-min 428
cargo build --workspace --bins --all-features
git diff --exit-code
```

Later minimum-compiler syntax and cost-disclosure changes are outside this
immutable checkpoint. Their affected source guards and UI acceptance are
verified separately; do not treat this result as a run of those later files.
