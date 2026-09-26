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

USAGE="usage: install.sh [--prefix PATH] [--senate-only | --full]"
PREFIX="${HOME:?HOME must be set}/.local"
MODE=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { echo "$USAGE" >&2; exit 2; }
            PREFIX="$2"
            shift 2
            ;;
        --senate-only|--full)
            [ -z "$MODE" ] || [ "$MODE" = "$1" ] || {
                echo "install.sh: choose one of --senate-only and --full" >&2
                exit 2
            }
            MODE="$1"
            shift
            ;;
        *)
            echo "$USAGE" >&2
            exit 2
            ;;
    esac
done
if [ -z "$MODE" ]; then
    MODE="--full"
    if [ -t 0 ]; then
        printf 'Just here for the Senate? It installs only the `senate` debate CLI, without the daemon, TUI, or workers. [y/N] ' >&2
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
[ -f "$ROOT/Cargo.toml" ] || {
    echo "install.sh: could not find the workspace root (expected $ROOT/Cargo.toml)" >&2
    exit 1
}

command -v cargo >/dev/null 2>&1 || {
    echo "install.sh: cargo (a stable Rust toolchain, 1.85+) is required; see https://rustup.rs" >&2
    exit 1
}
command -v git >/dev/null 2>&1 || {
    echo "install.sh: git is required" >&2
    exit 1
}
if [ "$MODE" = "--senate-only" ]; then
    BINARIES="senate"
else
    command -v npm >/dev/null 2>&1 || {
        echo "install.sh: npm is required to run the operator TUI from a source checkout" >&2
        exit 1
    }
    # `heph` is the launcher; the rest are the binaries it and `hephaestus`
    # expect to find beside it (mirrors scripts/package_macos.py's BINARIES),
    # plus the standalone `senate`.
    BINARIES="heph hephaestus hephaestusd hephaestus-reference-worker hephaestus-reference-evaluator hephaestus-process-guardian senate"
fi

echo "Building $BINARIES (release)..." >&2
set -- cargo build --release --manifest-path "$ROOT/Cargo.toml"
for name in $BINARIES; do
    set -- "$@" --bin "$name"
done
"$@"

if [ "$MODE" = "--full" ]; then
    echo "Installing the operator TUI's dependencies..." >&2
    ( cd "$ROOT/apps/hephaestus-tui" && npm ci )
fi

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
    /*) ;;
    *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
RELEASE_DIR="$TARGET_DIR/release"

mkdir -p "$PREFIX/bin"
for name in $BINARIES; do
    BIN="$RELEASE_DIR/$name"
    [ -x "$BIN" ] || {
        echo "install.sh: expected build output missing: $BIN" >&2
        exit 1
    }
    LINK="$PREFIX/bin/$name"
    if [ -e "$LINK" ] || [ -L "$LINK" ]; then
        [ -L "$LINK" ] || {
            echo "install.sh: refusing to replace a non-symlink executable: $LINK" >&2
            exit 1
        }
        case "$(readlink "$LINK")" in
            "$BIN") ;;
            *)
                echo "install.sh: refusing to replace an unrelated symlink: $LINK" >&2
                exit 1
                ;;
        esac
    fi
    ln -sfn "$BIN" "$LINK"
done

if [ "$MODE" = "--senate-only" ]; then
    NEXT='senate ask "your question" --size M'
    echo >&2
    echo "Installed the Senate from $ROOT into $PREFIX/bin" >&2
else
    NEXT="heph"
    echo >&2
    echo "Installed Hephaestus from $ROOT into $PREFIX/bin" >&2
fi
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Add $PREFIX/bin to your PATH, then run: $NEXT" >&2 ;;
esac
echo "Run: $NEXT" >&2
