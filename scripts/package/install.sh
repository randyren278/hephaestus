#!/bin/sh
set -eu

PREFIX="${HOME:?HOME must be set}/.local"
if [ "$(uname -s)" != Darwin ]; then
    echo "this package installer requires macOS" >&2
    exit 1
fi
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
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PACKAGE="$ROOT/hephaestus"
[ -x "$PACKAGE/bin/hephaestus" ] || { echo "package binaries are missing" >&2; exit 1; }
[ -x "$PACKAGE/bin/node" ] || { echo "pinned Node runtime is missing" >&2; exit 1; }
[ -f "$PACKAGE/share/hephaestus/package.json" ] || { echo "package metadata is missing" >&2; exit 1; }
[ -f "$PACKAGE/share/hephaestus/architecture" ] || { echo "package architecture is missing" >&2; exit 1; }
PACKAGE_ARCH="$(cat "$PACKAGE/share/hephaestus/architecture")"
HOST_ARCH="$(uname -m)"
case "$HOST_ARCH:$PACKAGE_ARCH" in
    arm64:arm64|aarch64:arm64|x86_64:x86_64) ;;
    *) echo "package architecture $PACKAGE_ARCH does not match host $HOST_ARCH" >&2; exit 1 ;;
esac

RELEASE="$(basename "$ROOT")"
RELEASES="$PREFIX/share/hephaestus/releases"
DESTINATION="$RELEASES/$RELEASE"
mkdir -p "$RELEASES" "$PREFIX/bin"
if [ -e "$DESTINATION" ] || [ -L "$DESTINATION" ]; then
    echo "release is already installed: $DESTINATION" >&2
    exit 1
fi
STAGING="$(mktemp -d "$RELEASES/.install-$RELEASE.XXXXXX")"
cleanup() { rm -rf "$STAGING"; }
trap cleanup EXIT HUP INT TERM

for binary in hephaestus hephaestusd hephaestus-reference-worker hephaestus-reference-evaluator hephaestus-process-guardian; do
    LINK="$PREFIX/bin/$binary"
    EXPECTED="../share/hephaestus/current/bin/$binary"
    if [ -e "$LINK" ] || [ -L "$LINK" ]; then
        [ -L "$LINK" ] || { echo "refusing to replace a non-symlink executable: $LINK" >&2; exit 1; }
        [ "$(readlink "$LINK")" = "$EXPECTED" ] || { echo "refusing to replace an unrelated symlink: $LINK" >&2; exit 1; }
    fi
done
if [ -e "$PREFIX/share/hephaestus/current" ] && [ ! -L "$PREFIX/share/hephaestus/current" ]; then
    echo "refusing to replace a non-symlink current release path" >&2
    exit 1
fi

cp -R "$PACKAGE/." "$STAGING/"
chmod -R go-w "$STAGING"
mv "$STAGING" "$DESTINATION"
trap - EXIT HUP INT TERM

CURRENT_TEMP="$PREFIX/share/hephaestus/.current-$$"
ln -s "releases/$RELEASE" "$CURRENT_TEMP"
mv -fh "$CURRENT_TEMP" "$PREFIX/share/hephaestus/current"

for binary in hephaestus hephaestusd hephaestus-reference-worker hephaestus-reference-evaluator hephaestus-process-guardian; do
    LINK="$PREFIX/bin/$binary"
    EXPECTED="../share/hephaestus/current/bin/$binary"
    ln -sfn "$EXPECTED" "$LINK"
done

echo "Installed Hephaestus in $DESTINATION"
echo "Add $PREFIX/bin to PATH, then run: hephaestus init --fixture quickstart ./hephaestus-quickstart"
