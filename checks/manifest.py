"""Shared loader for the full-reign check manifest.

One file describes both gates -- the mutation set and the per-module coverage
floor -- so a module can never be added to one and forgotten in the other.

Accepts YAML (.yaml/.yml) or JSON (.json). JSON needs no third-party
dependency, which is why the Node/Go/Rust templates prefer it: those CI jobs
install Python for these checks and nothing else.

Schema:

    test_command: "python -m pytest -q -x"   # optional, --test-cmd overrides
    timeout_seconds: 120                      # optional
    mutation_timeout_seconds:                 # optional, longest prefix wins
      "crates/slow-package/": 240
    coverage_floor: 95.0                      # optional, --floor overrides
    critical_modules:                         # optional (coverage_gate only)
      - src/auth.ts
    mutations:                                # required (mutation_guard only)
      - id: unique-slug
        file: src/auth.ts
        invariant: "One sentence, in English, of what must always hold."
        find: "exact source substring, must match EXACTLY once"
        replace: "the broken version"
        test_cmd: "npm test -- auth.spec.ts"   # optional, narrowest command wins
"""
from __future__ import annotations

import json
import pathlib

MUTATION_FIELDS = frozenset({"id", "file", "find", "replace", "invariant"})


class ManifestError(RuntimeError):
    pass


def load(path: pathlib.Path) -> dict:
    """Parse the manifest, or raise ManifestError with an actionable message."""
    try:
        text = path.read_text()
    except OSError as error:
        raise ManifestError(f"cannot read {path}: {error}") from error

    if path.suffix == ".json":
        try:
            data = json.loads(text)
        except json.JSONDecodeError as error:
            raise ManifestError(f"{path} is not valid JSON: {error}") from error
    else:
        try:
            import yaml
        except ImportError as error:
            raise ManifestError(
                f"{path} is YAML but PyYAML is not installed. Either "
                f"`pip install pyyaml` or convert the manifest to .json, which "
                f"needs no dependency."
            ) from error
        try:
            data = yaml.safe_load(text)
        except yaml.YAMLError as error:
            raise ManifestError(f"{path} is not valid YAML: {error}") from error

    if not isinstance(data, dict):
        raise ManifestError(f"{path} must be a mapping at the top level")
    return data


def mutations(data: dict, path: pathlib.Path) -> list[dict]:
    """Return the validated mutation list, or raise ManifestError."""
    entries = data.get("mutations")
    if not entries:
        raise ManifestError(f"{path} has no 'mutations' list")
    if not isinstance(entries, list):
        raise ManifestError(f"{path}: 'mutations' must be a list")

    seen: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise ManifestError(f"{path}: every mutation must be a mapping, got {entry!r}")
        missing = MUTATION_FIELDS - entry.keys()
        if missing:
            raise ManifestError(
                f"mutation {entry.get('id', '?')!r} missing fields: {sorted(missing)}")
        # A duplicate id makes the run log ambiguous and lets one entry silently
        # stand in for another when the count is ratcheted.
        if entry["id"] in seen:
            raise ManifestError(f"{path}: duplicate mutation id {entry['id']!r}")
        seen.add(entry["id"])
        if entry["find"] == entry["replace"]:
            raise ManifestError(
                f"mutation {entry['id']!r}: 'find' and 'replace' are identical, so "
                f"nothing is broken and the KILLED verdict would be meaningless")
        test_cmd = entry.get("test_cmd")
        if test_cmd is not None and (not isinstance(test_cmd, str) or not test_cmd.strip()):
            raise ManifestError(
                f"mutation {entry['id']!r}: 'test_cmd' must be a non-empty string")
    return entries
