"""Check mutation source anchors before starting the expensive mutation matrix."""

import argparse
import pathlib

from manifest import ManifestError, load, mutations


def stale_targets(manifest: pathlib.Path, root: pathlib.Path) -> list[str]:
    """Return every mutation whose source anchor is missing or ambiguous."""
    stale = []
    for entry in mutations(load(manifest), manifest):
        path = root / entry["file"]
        matches = path.read_text().count(entry["find"]) if path.is_file() else 0
        if matches != 1:
            stale.append(f"{entry['id']}: {entry['file']} matched {matches} times")
    return stale


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--root", type=pathlib.Path, default=pathlib.Path("."))
    args = parser.parse_args()
    try:
        stale = stale_targets(args.manifest, args.root)
    except (ManifestError, OSError) as error:
        parser.error(str(error))
    if stale:
        print("\n".join(stale))
        return 1
    print("all mutation source anchors match exactly once")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
