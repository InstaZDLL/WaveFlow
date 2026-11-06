#!/usr/bin/env python3
"""Point an updater manifest at a copy of its payloads on another host.

    mirror-manifest.py --manifest latest.json --assets <dir> \\
        --base-url https://download.waveflow.app/releases/v2.0.0 --out <file>

Every platform's `url` becomes `<base-url>/<the file name it ends with>`.
Nothing else changes: the signature covers the payload's bytes, not the
address it was fetched from, so the copy verifies under the same public
key as the original.

Refuses rather than guesses. Each payload has to be a file in `--assets`
(the directory about to be uploaded), so a manifest can never point at
something the mirror does not hold; and each signature has to be the
`.sig` file shipped next to it, so the manifest cannot vouch for bytes
other than those.

`--self-test` runs the checks against a manifest it builds itself.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tempfile
from urllib.parse import unquote, urlsplit


class Refused(Exception):
    pass


def payload_name(url: str) -> str:
    path = urlsplit(url).path
    name = unquote(path.rsplit("/", 1)[-1])
    if not name or name in {".", ".."} or "/" in name or "\\" in name:
        raise Refused(f"no file name at the end of {url!r}")
    return name


def mirror(manifest: dict, assets: pathlib.Path, base_url: str) -> dict:
    base_url = base_url.rstrip("/")
    if not base_url.startswith("https://"):
        raise Refused(f"the base URL must be https (got {base_url!r}): the updater refuses anything else")
    platforms = manifest.get("platforms")
    if not isinstance(platforms, dict) or not platforms:
        raise Refused("the manifest has no platforms")

    out = dict(manifest)
    out["platforms"] = {}
    for platform, entry in platforms.items():
        name = payload_name(entry["url"])
        payload = assets / name
        if not payload.is_file():
            raise Refused(f"{platform}: {name} is not in {assets}")
        sig = assets / f"{name}.sig"
        if not sig.is_file():
            raise Refused(f"{platform}: {name}.sig is not in {assets}")
        if sig.read_text(encoding="utf-8").strip() != entry["signature"]:
            raise Refused(f"{platform}: the signature in the manifest is not the one in {name}.sig")
        out["platforms"][platform] = {**entry, "url": f"{base_url}/{name}"}
    return out


def self_test() -> int:
    failures = 0

    def expect(label: str, ok: bool) -> None:
        nonlocal failures
        print(("ok   " if ok else "FAIL ") + label)
        failures += not ok

    with tempfile.TemporaryDirectory() as tmp:
        assets = pathlib.Path(tmp)
        (assets / "WaveFlow_2.0.0_linux-x86_64.AppImage").write_bytes(b"x")
        (assets / "WaveFlow_2.0.0_linux-x86_64.AppImage.sig").write_text("SIG\n", encoding="utf-8")
        gh = "https://github.com/o/r/releases/download/v2.0.0"
        manifest = {
            "version": "2.0.0",
            "notes": "",
            "pub_date": "2026-11-06T00:00:00.000Z",
            "platforms": {
                "linux-x86_64": {"signature": "SIG", "url": f"{gh}/WaveFlow_2.0.0_linux-x86_64.AppImage"},
            },
        }
        base = "https://download.example/releases/v2.0.0/"
        out = mirror(manifest, assets, base)
        expect(
            "url rewritten onto the base, trailing slash dropped",
            out["platforms"]["linux-x86_64"]["url"]
            == "https://download.example/releases/v2.0.0/WaveFlow_2.0.0_linux-x86_64.AppImage",
        )
        expect("signature and version kept", out["platforms"]["linux-x86_64"]["signature"] == "SIG"
               and out["version"] == "2.0.0")
        expect("input left untouched", manifest["platforms"]["linux-x86_64"]["url"].startswith(gh))

        def refused(label: str, m: dict, base_url: str = base) -> None:
            try:
                mirror(m, assets, base_url)
            except Refused:
                expect(label, True)
            else:
                expect(label, False)

        refused("http base refused", manifest, "http://download.example/x")
        missing = json.loads(json.dumps(manifest))
        missing["platforms"]["linux-x86_64"]["url"] = f"{gh}/Other.AppImage"
        refused("payload absent from the assets refused", missing)
        wrong_sig = json.loads(json.dumps(manifest))
        wrong_sig["platforms"]["linux-x86_64"]["signature"] = "OTHER"
        refused("signature differing from the .sig refused", wrong_sig)
        dotdot = json.loads(json.dumps(manifest))
        dotdot["platforms"]["linux-x86_64"]["url"] = f"{gh}/%2E%2E"
        refused("dot segment refused", dotdot)
        refused("empty platforms refused", {**manifest, "platforms": {}})

    print("self-test", "failed" if failures else "passed")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--manifest", type=pathlib.Path)
    parser.add_argument("--assets", type=pathlib.Path)
    parser.add_argument("--base-url")
    parser.add_argument("--out", type=pathlib.Path)
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if not (args.manifest and args.assets and args.base_url and args.out):
        parser.error("--manifest, --assets, --base-url and --out are required")

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    try:
        out = mirror(manifest, args.assets, args.base_url)
    except Refused as err:
        print(f"::error::{err}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")
    for platform, entry in out["platforms"].items():
        print(f"{platform}: {entry['url']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
