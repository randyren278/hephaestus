#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if (( $# > 1 )); then
    echo "usage: scripts/check-fast.sh [cargo-package]" >&2
    exit 2
fi

cargo fmt --all -- --check
if (( $# == 1 )); then
    cargo clippy -p "$1" --all-targets --all-features -- -D warnings
else
    cargo clippy --workspace --all-targets --all-features -- -D warnings
fi
