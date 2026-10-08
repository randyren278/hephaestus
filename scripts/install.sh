#!/bin/sh
# Source-checkout installer for Hephaestus.
#
# Builds the workspace's binaries in release mode and symlinks them —
# including the `heph` launcher — into a user-local prefix (default
# ~/.local/bin). Installs the operator TUI's npm dependencies so `heph`/
# `hephaestus tui` can run it straight from this checkout; nothing is
# compiled or bundled for the TUI, it runs from source via `tsx`.
#
# Never uses sudo and never writes outside this checkout's own `target/`
# directory (or $CARGO_TARGET_DIR) and the chosen --prefix. Safe to re-run:
# it only ever replaces symlinks it created itself, pointing at this
# checkout's freshly built binaries.
#
# Usage: scripts/install.sh [--prefix PATH] [--senate-only | --full]
#   PATH defaults to ~/.local; its bin/ directory should be on PATH.
#   --senate-only installs just the `senate` debate CLI (its personas are
#   compiled in): no daemon, TUI, workers, or npm. --full installs everything.
#   With neither flag, an interactive run asks; a non-interactive one is full.
set -eu

# --- output styling ---------------------------------------------------
# Same colosseum palette as the operator TUI (apps/hephaestus-tui/src/theme.ts)
# and the pixel art (scripts/pixel_art/generate.py): ember/emberBright for the
# Champion, gold for the judge/seal, gray for the challenger, danger for
# failures. Truecolor only when stderr is a real, non-dumb TTY and NO_COLOR
# is unset, unless FORCE_COLOR/CLICOLOR_FORCE says otherwise; plain text
# (no escape codes at all) everywhere else, so piped output, CI, and the
# test suite stay exactly as readable as before.
COLOR=0
if [ -t 2 ] && [ "${TERM:-}" != "dumb" ] && [ -z "${NO_COLOR:-}" ]; then
    COLOR=1
fi
if [ -n "${FORCE_COLOR:-}" ] && [ "${FORCE_COLOR:-0}" != "0" ]; then
    COLOR=1
fi
if [ -n "${CLICOLOR_FORCE:-}" ] && [ "${CLICOLOR_FORCE:-0}" != "0" ]; then
    COLOR=1
fi

if [ "$COLOR" = 1 ]; then
    ESC="$(printf '\033')"
    C_RESET="${ESC}[0m"
    C_BOLD="${ESC}[1m"
    C_EMBER="${ESC}[38;2;232;89;12m"
    C_EMBERBR="${ESC}[38;2;255;138;61m"
    C_GOLD="${ESC}[38;2;244;197;66m"
    C_GRAY="${ESC}[38;2;91;98;112m"
    C_DANGER="${ESC}[38;2;192;57;43m"
    C_BORDER="${ESC}[38;2;58;63;77m"
    C_INKDIM="${ESC}[38;2;162;167;181m"
else
    C_RESET=""; C_BOLD=""; C_EMBER=""; C_EMBERBR=""; C_GOLD=""
    C_GRAY=""; C_DANGER=""; C_BORDER=""; C_INKDIM=""
fi

# Unicode glyphs when the locale claims UTF-8, plain ASCII otherwise.
UNICODE=0
case "${LC_ALL:-}${LANG:-}" in
    *UTF-8*|*UTF8*|*utf-8*|*utf8*) UNICODE=1 ;;
esac
if [ "$UNICODE" = 1 ]; then
    G_CHAMPION="▲"; G_CHALLENGER="◆"; G_CARET="›"; G_SEAL="◈"
    G_RULE="━"; G_CHECK="✓"; G_CROSS="✕"
else
    G_CHAMPION="^"; G_CHALLENGER="<>"; G_CARET=">"; G_SEAL="*"
    G_RULE="="; G_CHECK="+"; G_CROSS="x"
fi

rule() {
    # rule N -> N copies of $G_RULE, no trailing newline.
    i=0
    while [ "$i" -lt "$1" ]; do
        printf '%s' "$G_RULE"
        i=$((i + 1))
    done
}

step() { printf '%s%s %s%s\n' "$C_GOLD" "$G_CARET" "$*" "$C_RESET" >&2; }
ok() { printf '  %s%s %s%s\n' "$C_EMBERBR" "$G_CHECK" "$*" "$C_RESET" >&2; }
fail() { printf '%s%s install.sh: %s%s\n' "$C_DANGER" "$G_CROSS" "$*" "$C_RESET" >&2; exit 1; }
usage_fail() { printf '%s%s install.sh: %s%s\n' "$C_DANGER" "$G_CROSS" "$*" "$C_RESET" >&2; exit 2; }

USAGE="usage: install.sh [--prefix PATH] [--senate-only | --full]"
PREFIX="${HOME:?HOME must be set}/.local"
MODE=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { printf '%s%s%s\n' "$C_DANGER" "$USAGE" "$C_RESET" >&2; exit 2; }
            PREFIX="$2"
            shift 2
            ;;
        --senate-only|--full)
            [ -z "$MODE" ] || [ "$MODE" = "$1" ] || usage_fail "choose one of --senate-only and --full"
            MODE="$1"
            shift
            ;;
        *)
            printf '%s%s%s\n' "$C_DANGER" "$USAGE" "$C_RESET" >&2
            exit 2
            ;;
    esac
done
if [ -z "$MODE" ]; then
    MODE="--full"
    if [ -t 0 ]; then
        printf '%s%s Just here for the Senate? It installs only the `senate` debate CLI, without the daemon, TUI, or workers. [y/N] %s' \
            "$C_GOLD" "$G_SEAL" "$C_RESET" >&2
        read -r ANSWER || ANSWER=""
        case "$ANSWER" in
            [yY]|[yY][eE][sS]) MODE="--senate-only" ;;
        esac
    fi
fi
case "$PREFIX" in
    /*) ;;
    *) PREFIX="$(pwd)/$PREFIX" ;;
esac

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
[ -f "$ROOT/Cargo.toml" ] || fail "could not find the workspace root (expected $ROOT/Cargo.toml)"

printf '\n%s%s %sHEPHAESTUS%s\n' "$C_EMBER" "$G_CHAMPION" "$C_BOLD$C_EMBERBR" "$C_RESET" >&2
if [ "$MODE" = "--senate-only" ]; then
    printf '%s  the Senate debate CLI%s\n' "$C_INKDIM" "$C_RESET" >&2
else
    printf '%s  the forge that duels its own code%s\n' "$C_INKDIM" "$C_RESET" >&2
fi
printf '%s%s%s\n\n' "$C_BORDER" "$(rule 42)" "$C_RESET" >&2

step "Checking toolchain"
command -v cargo >/dev/null 2>&1 || fail "cargo (a stable Rust toolchain, 1.88+) is required; see https://rustup.rs"
CARGO_V="$(cargo --version 2>/dev/null | head -n1)"
ok "cargo${CARGO_V:+ ($CARGO_V)}"
command -v git >/dev/null 2>&1 || fail "git is required"
GIT_V="$(git --version 2>/dev/null | head -n1)"
ok "git${GIT_V:+ ($GIT_V)}"
if [ "$MODE" = "--senate-only" ]; then
    BINARIES="senate"
else
    command -v npm >/dev/null 2>&1 || fail "npm is required to run the operator TUI from a source checkout"
    NPM_V="$(npm --version 2>/dev/null | head -n1)"
    ok "npm${NPM_V:+ ($NPM_V)}"
    # `heph` is the launcher; the rest are the binaries it and `hephaestus`
    # expect to find beside it (mirrors scripts/package_macos.py's BINARIES),
    # plus the standalone `senate`.
    BINARIES="heph hephaestus hephaestusd hephaestus-reference-worker hephaestus-reference-evaluator hephaestus-process-guardian senate"
fi
echo >&2

BIN_COUNT=0
for name in $BINARIES; do BIN_COUNT=$((BIN_COUNT + 1)); done
BIN_NOUN="binaries"
[ "$BIN_COUNT" = 1 ] && BIN_NOUN="binary"
step "Building $BIN_COUNT $BIN_NOUN (release)"
for name in $BINARIES; do
    printf '  %s%s%s %s\n' "$C_INKDIM" "$G_CHALLENGER" "$C_RESET" "$name" >&2
done
BUILD_START="$(date +%s)"
set -- cargo build --release --manifest-path "$ROOT/Cargo.toml"
for name in $BINARIES; do
    set -- "$@" --bin "$name"
done
"$@"
BUILD_ELAPSED=$(($(date +%s) - BUILD_START))
ok "built in ${BUILD_ELAPSED}s"
echo >&2

if [ "$MODE" = "--full" ]; then
    step "Installing the operator TUI's dependencies"
    NPM_START="$(date +%s)"
    ( cd "$ROOT/apps/hephaestus-tui" && npm ci )
    NPM_ELAPSED=$(($(date +%s) - NPM_START))
    ok "installed in ${NPM_ELAPSED}s"
    echo >&2
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
RELEASE_DIR="$TARGET_DIR/release"

step "Linking into $PREFIX/bin"
mkdir -p "$PREFIX/bin"
for name in $BINARIES; do
    BIN="$RELEASE_DIR/$name"
    [ -x "$BIN" ] || fail "expected build output missing: $BIN"
    LINK="$PREFIX/bin/$name"
    if [ -e "$LINK" ] || [ -L "$LINK" ]; then
        [ -L "$LINK" ] || fail "refusing to replace a non-symlink executable: $LINK"
        case "$(readlink "$LINK")" in
            "$BIN") ;;
            *) fail "refusing to replace an unrelated symlink: $LINK" ;;
        esac
    fi
    ln -sfn "$BIN" "$LINK"
    ok "$name"
done
echo >&2

if [ "$MODE" = "--senate-only" ]; then
    NEXT='senate ask "your question" --size M'
    INSTALLED_WHAT="the Senate"
else
    NEXT="heph"
    INSTALLED_WHAT="Hephaestus"
fi
printf '%s%s%s\n' "$C_BORDER" "$(rule 42)" "$C_RESET" >&2
printf ' %s%s%s %sInstalled %s%s\n' "$C_GOLD" "$G_SEAL" "$C_RESET" "$C_BOLD" "$INSTALLED_WHAT" "$C_RESET" >&2
printf '   from %s\n' "$ROOT" >&2
printf '   into %s/bin\n' "$PREFIX" >&2
printf '%s%s%s\n' "$C_BORDER" "$(rule 42)" "$C_RESET" >&2
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) printf '%sAdd %s/bin to your PATH, then run: %s%s\n' "$C_DANGER" "$PREFIX" "$NEXT" "$C_RESET" >&2 ;;
esac
printf '%s%sRun: %s%s\n' "$C_BOLD" "$C_GOLD" "$NEXT" "$C_RESET" >&2
