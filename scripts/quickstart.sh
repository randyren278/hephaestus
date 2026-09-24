#!/usr/bin/env bash
# One-command tour of the Hephaestus control plane.
#
# Builds the workspace, starts a daemon on a scratch data directory, registers a
# World and two Genomes (parent + child) from examples/quickstart, runs the
# parent through the offline reference runtime, measures parent versus child in
# the protected Arena, verifies replay, restarts the daemon to prove everything
# was canonical history, and stops it.
#
# Usage: scripts/quickstart.sh [data-dir]
#   data-dir defaults to ./.quickstart (created mode 0700, safe to delete).
#
# Requires: macOS (the daemon refuses to launch candidates without an OS
# sandbox; only Seatbelt is supported today), git, and a stable Rust toolchain.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="${1:-$ROOT/.quickstart}"
EXAMPLES="$ROOT/examples/quickstart"

step() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
show() { printf '  \033[2m$ %s\033[0m\n' "${*//$ROOT\//}" >&2; }
run()  { show "$*"; "$@"; }
# Run a command, echo its output, and return the first word (an identity).
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
H=("$BIN/hephaestus" --data-dir "$DATA")

if [ -e "$DATA/control.sock" ] && "${H[@]}" status >/dev/null 2>&1; then
  echo "A daemon is already serving $DATA; stopping it first."
  "${H[@]}" daemon stop >/dev/null
  sleep 1
fi
mkdir -p "$DATA" && chmod 700 "$DATA"
rm -rf "$DATA/work" && mkdir -p "$DATA/work"

step "Start the daemon (single writer, owner-only socket)"
show "hephaestusd --data-dir $DATA --source-repository $ROOT --evaluator-executable $BIN/hephaestus-reference-evaluator &"
"$BIN/hephaestusd" --data-dir "$DATA" --source-repository "$ROOT" \
  --evaluator-executable "$BIN/hephaestus-reference-evaluator" &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID" 2>/dev/null || true' EXIT
wait_ready
run "${H[@]}" status

step "Publish the World's evaluator artifacts into the content-addressed store"
VISIBLE=$(capture "${H[@]}" arena manifest "$EXAMPLES/tasks/visible.json")
SEALED=$(capture "${H[@]}" arena manifest "$EXAMPLES/tasks/sealed.json")
EVALUATOR=$(capture "${H[@]}" artifact put "$BIN/hephaestus-reference-evaluator")
VERIFIER=$(capture "${H[@]}" verifier)

step "Register the World (Laws, authority ceiling, promotion policy, evaluator bindings)"
sed -e "s/__VISIBLE_MANIFEST__/$VISIBLE/" -e "s/__SEALED_MANIFEST__/$SEALED/" \
    -e "s/__EVALUATOR__/$EVALUATOR/" -e "s/__VERIFIER__/$VERIFIER/" \
    "$EXAMPLES/world.template.json" > "$DATA/work/world.json"
WORLD=$(capture "${H[@]}" world register "$DATA/work/world.json")

step "Register Markdown Genomes: identity parent and an uppercase child"
PARENT=$(capture "${H[@]}" genome register "$EXAMPLES/agent.md" --world "$WORLD")
sed -e "s/__PARENT_ID__/$PARENT/" "$EXAMPLES/candidate.md" > "$DATA/work/candidate.md"
CANDIDATE=$(capture "${H[@]}" genome register "$DATA/work/candidate.md" --world "$WORLD")
run "${H[@]}" genome list
SOURCE_REVISION=$(git -C "$ROOT" rev-parse HEAD)
EVALUATION_ID="quickstart-$(printf '%s\n' "$WORLD" "$PARENT" "$CANDIDATE" "$SOURCE_REVISION" | git hash-object --stdin)"

step "Unfreeze (daemons start frozen; only the operator can lift it) and run the identity parent"
run "${H[@]}" unfreeze
run "${H[@]}" run "$PARENT"

step "Measure parent vs child in the protected Arena"
EVALUATION=$(run "${H[@]}" arena evaluate "$EVALUATION_ID" "$PARENT" "$CANDIDATE")
printf '%s\n' "$EVALUATION"
[[ "$EVALUATION" == *"candidate_visible=1/1"* && "$EVALUATION" == *"parent_visible=0/1"* ]] || {
  echo "Markdown reference instructions did not produce the expected Arena improvement" >&2; exit 1;
}

step "Compute and persist the operator-only selection receipt"
# The receipt prints measured metrics separately from the missing invariant
# gate; promotion must remain false until that independent proof exists.
SELECTION=$(run "${H[@]}" arena select "$EVALUATION_ID")
printf '%s\n' "$SELECTION"
[[ "$SELECTION" == *"correctness_improvements=2"* ]] || {
  echo "selection did not preserve the measured Markdown correctness improvement" >&2; exit 1;
}
[[ "$SELECTION" == *"invariant_gate_verified=false"* ]] || {
  echo "selection unexpectedly claims invariant verification" >&2; exit 1;
}
[[ "$SELECTION" == *"promotion_eligible=false"* ]] || {
  echo "selection unexpectedly permits promotion without invariant proof" >&2; exit 1;
}
SELECTION_RETRY=$(run "${H[@]}" arena select "$EVALUATION_ID")
[[ "$SELECTION_RETRY" == "$SELECTION" ]] || {
  echo "selection retry changed the canonical receipt" >&2; exit 1;
}

step "Replay the whole ledger and compare it with live state"
run "${H[@]}" replay

step "Crash the daemon and bring it back: everything above is canonical history"
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
"$BIN/hephaestusd" --data-dir "$DATA" --source-repository "$ROOT" \
  --evaluator-executable "$BIN/hephaestus-reference-evaluator" &
DAEMON_PID=$!
wait_ready
run "${H[@]}" status
run "${H[@]}" genome show "$CANDIDATE"
SELECTION_AFTER_RESTART=$(run "${H[@]}" arena select "$EVALUATION_ID")
[[ "$SELECTION_AFTER_RESTART" == "$SELECTION" ]] || {
  echo "selection changed after daemon restart" >&2; exit 1;
}

step "Stop"
run "${H[@]}" daemon stop
wait "$DAEMON_PID" 2>/dev/null || true
trap - EXIT
printf '\n\033[1;32m✔ Done.\033[0m Data directory: %s\n' "$DATA"
