#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo "macOS package acceptance requires macOS" >&2
  exit 2
fi
if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo "usage: scripts/package_macos_acceptance.sh PACKAGE.tar.gz" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
WORK="$(mktemp -d /tmp/hpkg.XXXXXX)"
DAEMON_PID=""
cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  if [[ "${KEEP_PACKAGE_ACCEPTANCE:-0}" == 1 ]]; then
    echo "preserved acceptance workspace: $WORK" >&2
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

mkdir -p "$WORK/unpack" "$WORK/home"
chmod 700 "$WORK/home"
tar -xzf "$ARCHIVE" -C "$WORK/unpack"
PACKAGE_ROOT="$(find "$WORK/unpack" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ -x "$PACKAGE_ROOT/install.sh" ]] || { echo "archive install.sh is missing" >&2; exit 1; }

HOME="$WORK/home" "$PACKAGE_ROOT/install.sh" --prefix "$WORK/home/.local"
mv "$WORK/home/.local" "$WORK/moved-local"
PREFIX="$WORK/moved-local"
export HOME="$WORK/home"
GIT_EXECUTABLE="$(command -v git)"
[[ -x "$GIT_EXECUTABLE" ]] || { echo "acceptance requires an installed git executable" >&2; exit 1; }
PYTHON_EXECUTABLE="$(command -v python3)"
[[ -x "$PYTHON_EXECUTABLE" ]] || { echo "acceptance requires Python 3 for the PTY harness" >&2; exit 1; }
echo "Acceptance prerequisite: host Git at $GIT_EXECUTABLE; host Node/npm are excluded from PATH."
mkdir -p "$WORK/tools"
ln -s "$GIT_EXECUTABLE" "$WORK/tools/git"
ln -s "$PYTHON_EXECUTABLE" "$WORK/tools/python3"
export PATH="$WORK/tools:$PREFIX/bin:/usr/bin:/bin"
CLI="$PREFIX/bin/hephaestus"
DAEMON="$PREFIX/bin/hephaestusd"
EVALUATOR="$PREFIX/share/hephaestus/current/bin/hephaestus-reference-evaluator"
DATA="$WORK/home/data"
FIXTURE="$WORK/home/quickstart"

"$CLI" --version
[[ "$("$PREFIX/share/hephaestus/current/bin/node" --version)" == v24.21.0 ]] || { echo "bundled Node version is wrong" >&2; exit 1; }
if command -v npm >/dev/null 2>&1 || command -v node >/dev/null 2>&1; then
  echo "acceptance PATH unexpectedly exposes a host Node/npm" >&2
  exit 1
fi
"$CLI" init --fixture quickstart "$FIXTURE"
[[ -f "$FIXTURE/world.template.json" ]]
[[ -f "$FIXTURE/agent.md" ]]
[[ -f "$FIXTURE/repository/.git/HEAD" ]]
[[ -n "$(git -C "$FIXTURE/repository" rev-parse HEAD)" ]]

"$DAEMON" --data-dir "$DATA" --source-repository "$FIXTURE/repository" \
  --evaluator-executable "$EVALUATOR" >"$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
ready=false
for _ in $(seq 1 200); do
  if "$CLI" --data-dir "$DATA" status >/dev/null 2>&1; then
    ready=true
    break
  fi
  kill -0 "$DAEMON_PID" 2>/dev/null || { cat "$WORK/daemon.log" >&2; exit 1; }
  sleep 0.05
done
[[ "$ready" == true ]] || { cat "$WORK/daemon.log" >&2; echo "packaged daemon did not start" >&2; exit 1; }

capture_id() {
  local output
  output=$("$CLI" --data-dir "$DATA" "$@")
  printf '%s\n' "$output" >&2
  awk 'NR == 1 { print $1; exit }' <<<"$output"
}

EXAMPLES="$FIXTURE"
VISIBLE=$(capture_id arena manifest "$EXAMPLES/tasks/visible.json")
SEALED=$(capture_id arena manifest "$EXAMPLES/tasks/sealed.json")
EVALUATOR_ID=$(capture_id artifact put "$EVALUATOR")
VERIFIER_ID=$(capture_id verifier)
mkdir -p "$DATA/work"
sed -e "s/__VISIBLE_MANIFEST__/$VISIBLE/" -e "s/__SEALED_MANIFEST__/$SEALED/" \
  -e "s/__EVALUATOR__/$EVALUATOR_ID/" -e "s/__VERIFIER__/$VERIFIER_ID/" \
  "$EXAMPLES/world.template.json" > "$DATA/work/world.json"
WORLD=$(capture_id world register "$DATA/work/world.json")
PARENT=$(capture_id genome register "$EXAMPLES/agent.md" --world "$WORLD")
sed "s/__PARENT_ID__/$PARENT/" "$EXAMPLES/candidate.md" > "$DATA/work/candidate.md"
CANDIDATE=$(capture_id genome register "$DATA/work/candidate.md" --world "$WORLD")
SOURCE_REVISION=$(git -C "$FIXTURE/repository" rev-parse HEAD)
EVALUATION_ID="package-$(printf '%s\n' "$WORLD" "$PARENT" "$CANDIDATE" "$SOURCE_REVISION" | git hash-object --stdin)"

"$CLI" --data-dir "$DATA" unfreeze
"$CLI" --data-dir "$DATA" run "$PARENT"
EVALUATION=$("$CLI" --data-dir "$DATA" arena evaluate "$EVALUATION_ID" "$PARENT" "$CANDIDATE")
printf '%s\n' "$EVALUATION"
[[ "$EVALUATION" == *"candidate_visible=1/1"* && "$EVALUATION" == *"parent_visible=0/1"* ]] || {
  echo "installed Arena fixture did not produce the expected visible improvement" >&2
  exit 1
}
SELECTION=$("$CLI" --data-dir "$DATA" arena select "$EVALUATION_ID")
printf '%s\n' "$SELECTION"
[[ "$SELECTION" == *"correctness_improvements=2"* && "$SELECTION" == *"promotion_eligible=false"* ]] || {
  echo "installed selection receipt did not preserve its measured and fail-closed outcome" >&2
  exit 1
}
"$CLI" --data-dir "$DATA" replay
"$WORK/tools/python3" "$ROOT/scripts/package/tui_acceptance.py" "$CLI" "$DATA" "$HOME" "$PATH"
"$CLI" --data-dir "$DATA" daemon stop
wait "$DAEMON_PID"
DAEMON_PID=""
echo "Isolated-home macOS package install, relocation, fixture init, offline Arena, replay, and bundled TUI acceptance passed (host Git prerequisite)."
