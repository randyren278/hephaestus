#!/usr/bin/env bash
# Self-dogfooding demo for roadmap item 15: a sandboxed Hephaestus lineage
# proposes and validates an improvement to a Hephaestus source file (here, a
# small documentation fixture) and the result is a local branch a human must
# review and merge. See examples/self_dogfood/README.md and
# docs/SELF_DOGFOODING.md for the full write-up of what this proves and does
# not prove.
#
# This never touches the real Hephaestus checkout's own tracked files: it
# copies examples/self_dogfood/repository into a disposable scratch
# directory and runs the whole loop there.
#
# Usage: scripts/self_dogfood.sh [work-dir]
#   work-dir defaults to ./.self-dogfood (created mode 0700, safe to delete).
#
# Requires: macOS (the daemon refuses to launch candidates without an OS
# sandbox; only Seatbelt is supported today), git, and a stable Rust toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${1:-$ROOT/.self-dogfood}"
FIXTURE="$ROOT/examples/self_dogfood"

step() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
show() { printf '  \033[2m$ %s\033[0m\n' "${*//$ROOT\//}" >&2; }
run()  { show "$*"; "$@"; }
wait_ready() {
  for _ in $(seq 1 200); do
    "${H[@]}" status >/dev/null 2>&1 && return 0
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon exited during startup" >&2; exit 1; }
    sleep 0.05
  done
  echo "daemon did not become ready" >&2; exit 1
}
capture() { local out; out=$(run "$@"); printf '%s\n' "$out" >&2; awk '{print $1; exit}' <<<"$out"; }

step "Build"
run cargo build --release --workspace
BIN="$ROOT/target/release"

step "Create a disposable copy of the fixture (the real checkout is never touched)"
rm -rf "$WORK"
mkdir -p "$WORK/data" "$WORK/work"
chmod 700 "$WORK/data"
REPO="$WORK/repository"
cp -R "$FIXTURE/repository" "$REPO"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.name "Hephaestus Self-Dogfood"
git -C "$REPO" config user.email "hephaestus-self-dogfood@example.invalid"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "self-dogfood fixture: initial NOTE.md"
[[ -z "$(git -C "$REPO" remote)" ]] || { echo "fixture repository unexpectedly has a remote" >&2; exit 1; }
DATA="$WORK/data"
H=("$BIN/hephaestus" --data-dir "$DATA")

step "Start the daemon against the disposable fixture repository (single writer, owner-only socket)"
show "hephaestusd --data-dir $DATA --source-repository $REPO --evaluator-executable $BIN/hephaestus-reference-evaluator &"
"$BIN/hephaestusd" --data-dir "$DATA" --source-repository "$REPO" \
  --evaluator-executable "$BIN/hephaestus-reference-evaluator" &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID" 2>/dev/null || true' EXIT
wait_ready
run "${H[@]}" status

step "Publish the World's evaluator artifacts into the content-addressed store"
VISIBLE=$(capture "${H[@]}" arena manifest "$FIXTURE/tasks/visible.json")
SEALED=$(capture "${H[@]}" arena manifest "$FIXTURE/tasks/sealed.json")
EVALUATOR=$(capture "${H[@]}" artifact put "$BIN/hephaestus-reference-evaluator")
VERIFIER=$(capture "${H[@]}" verifier)

step "Register the World"
sed -e "s/__VISIBLE_MANIFEST__/$VISIBLE/" -e "s/__SEALED_MANIFEST__/$SEALED/" \
    -e "s/__EVALUATOR__/$EVALUATOR/" -e "s/__VERIFIER__/$VERIFIER/" \
    "$FIXTURE/world.template.json" > "$WORK/work/world.json"
WORLD=$(capture "${H[@]}" world register "$WORK/work/world.json")

step "Register the identity parent and ascii_uppercase candidate Genomes"
PARENT=$(capture "${H[@]}" genome register "$FIXTURE/agent.md" --world "$WORLD")
sed -e "s/__PARENT_ID__/$PARENT/" "$FIXTURE/candidate.md" > "$WORK/work/candidate.md"
CANDIDATE=$(capture "${H[@]}" genome register "$WORK/work/candidate.md" --world "$WORLD")
SOURCE_REVISION=$(git -C "$REPO" rev-parse HEAD)
EVALUATION_ID="self-dogfood-$(printf '%s\n' "$WORLD" "$PARENT" "$CANDIDATE" "$SOURCE_REVISION" | git hash-object --stdin)"

step "Unfreeze and run the identity parent"
run "${H[@]}" unfreeze
run "${H[@]}" run "$PARENT"

step "Measure parent vs candidate in the protected Arena"
EVALUATION=$(run "${H[@]}" arena evaluate "$EVALUATION_ID" "$PARENT" "$CANDIDATE")
printf '%s\n' "$EVALUATION"
[[ "$EVALUATION" == *"candidate_visible=1/1"* && "$EVALUATION" == *"parent_visible=0/1"* ]] || {
  echo "the candidate did not measurably improve on the fixture task" >&2; exit 1;
}

step "Compute and replay-verify the operator-only selection receipt (evidence, not a trusted claim)"
SELECTION=$(run "${H[@]}" arena select "$EVALUATION_ID")
printf '%s\n' "$SELECTION"
[[ "$SELECTION" == *"correctness_improvements=2"* ]] || {
  echo "selection did not preserve the measured correctness improvement" >&2; exit 1;
}
run "${H[@]}" replay

step "Only now, materialize the Arena-verified transform into the fixture as a proposal branch"
# This runs as ordinary unprivileged shell code, after and because of the
# evidence above -- not as an action taken by the candidate sandbox itself.
# It applies exactly the transform the Arena already proved correct
# (ascii_uppercase) to the same heading line the tasks measured, nothing more.
BRANCH="hephaestus/self-dogfood-proposal"
git -C "$REPO" checkout -q -b "$BRANCH"
python3 - "$REPO/NOTE.md" <<'PY'
import sys
path = sys.argv[1]
lines = open(path, encoding="utf-8").read().splitlines(keepends=True)
lines[0] = lines[0].upper()
open(path, "w", encoding="utf-8").writelines(lines)
PY
git -C "$REPO" add NOTE.md
git -C "$REPO" commit -q -m "self-dogfood: apply Arena-verified ascii_uppercase to NOTE.md heading

Evaluation: $EVALUATION_ID
World: $WORLD
Parent Genome: $PARENT
Candidate Genome: $CANDIDATE
Selection receipt: verified, promotion_eligible=false (see 'arena select' output above)

A human must review and merge this branch. Nothing in this pipeline pushed
or merged it automatically."
[[ -z "$(git -C "$REPO" remote)" ]] || { echo "fixture repository unexpectedly gained a remote" >&2; exit 1; }
git -C "$REPO" checkout -q main

step "Stop"
run "${H[@]}" daemon stop
wait "$DAEMON_PID" 2>/dev/null || true
trap - EXIT

printf '\n\033[1;32m✔ Done.\033[0m Fixture repository: %s\n' "$REPO"
printf 'Proposal branch (not merged, not pushed -- review it yourself):\n'
printf '  git -C %s log --oneline main..%s\n' "$REPO" "$BRANCH"
printf '  git -C %s diff main %s -- NOTE.md\n' "$REPO" "$BRANCH"
