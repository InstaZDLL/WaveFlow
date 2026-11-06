#!/usr/bin/env python3
"""Merge the per-platform updater manifests into one `latest.json`.

    merge-manifests.py <parts dir> <output file>

Reads every `latest-*.json` the platform jobs wrote with
`prepare-assets.py`. They must agree on the version and may not name
the same platform twice; anything else is an error, because a manifest
that mixes two builds would offer one version and install another.
"""

from __future__ import annotations

import json
import pathlib
import sys


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    parts_dir, output = pathlib.Path(argv[1]), pathlib.Path(argv[2])

    paths = sorted(parts_dir.glob("latest-*.json"))
    if not paths:
        print("::error::No per-platform manifests found", file=sys.stderr)
        return 1

    merged = None
    platforms = {}
    for path in paths:
        data = json.loads(path.read_text(encoding="utf-8"))
        if merged is None:
            merged = {k: v for k, v in data.items() if k != "platforms"}
        elif data.get("version") != merged.get("version"):
            print(
                f"::error::Manifest version mismatch: "
                f"{merged.get('version')!r} != {data.get('version')!r}",
                file=sys.stderr,
            )
            return 1
        platform_dict = data.get("platforms")
        if not isinstance(platform_dict, dict) or not platform_dict:
            print(f"::error::Manifest {path} has no platforms entry", file=sys.stderr)
            return 1
        overlap = set(platforms).intersection(platform_dict)
        if overlap:
            print(f"::error::Duplicate platform entries: {sorted(overlap)}", file=sys.stderr)
            return 1
        platforms.update(platform_dict)

    merged["platforms"] = platforms
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(merged, indent=2) + "\n", encoding="utf-8")
    print(f"Merged manifest -> {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
