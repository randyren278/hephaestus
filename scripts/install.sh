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
# Usage: scripts/install.sh [--prefix PATH]
#   PATH defaults to ~/.local; its bin/ directory should be on PATH.
set -eu

PREFIX="${HOME:?HOME must be set}/.local"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { echo "usage: install.sh [--prefix PATH]" >&2; exit 2; }
            PREFIX="$2"
            shift 2
            ;;
        *)
            echo "usage: install.sh [--prefix PATH]" >&2
            exit 2
            ;;
    esac
done
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
command -v npm >/dev/null 2>&1 || {
    echo "install.sh: npm is required to run the operator TUI from a source checkout" >&2
    exit 1
}

# `heph` is the launcher; the rest are the binaries it and `hephaestus`
# expect to find beside it (mirrors scripts/package_macos.py's BINARIES).
BINARIES="heph hephaestus hephaestusd hephaestus-reference-worker hephaestus-reference-evaluator hephaestus-process-guardian"

echo "Building Hephaestus (release)..." >&2
set -- cargo build --release --manifest-path "$ROOT/Cargo.toml"
for name in $BINARIES; do
    set -- "$@" --bin "$name"
done
"$@"

echo "Installing the operator TUI's dependencies..." >&2
( cd "$ROOT/apps/hephaestus-tui" && npm ci )

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

echo >&2
echo "Installed Hephaestus from $ROOT into $PREFIX/bin" >&2
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Add $PREFIX/bin to your PATH, then run: heph" >&2 ;;
esac
echo "Run: heph" >&2
