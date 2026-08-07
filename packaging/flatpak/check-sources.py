#!/usr/bin/env python3
"""Verify the committed Flatpak source manifests still cover the lockfiles.

Flathub builds with the network disabled, so every crate and every npm
tarball the build pulls must be declared upfront in `generated/*.json`.
A lockfile bump that forgets to re-run `generate-sources.sh` therefore
doesn't fail here — it fails much later, inside Flathub's sandbox, on a
crate nobody can download.

That is exactly what happened before this script existed: `chore: update
dependencies` (23be643) bumped `Cargo.lock` and left 53 crates with no
source entry, wasmtime 46.0.1 declared against 47.0.3 in the lock.

What this checks is **coverage, not freshness**: every registry crate in
`Cargo.lock` has a source entry, and every registry package in the
Flatpak `package-lock.json` has a tarball. Deliberately *not* "regenerate
and diff the tree" — `generate-sources.sh` runs `npm install
--package-lock-only`, which resolves `^` ranges against the registry as
it is right now, so that check would go red the moment any transitive
dependency publishes, with nothing wrong in the repo. Refreshing is the
scheduled workflow's job; this one only guards the invariant the build
actually needs.

Runs offline in about a second: no network, no venv, no npm.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
GEN_DIR = REPO_ROOT / "packaging" / "flatpak" / "generated"
CARGO_LOCK = REPO_ROOT / "src-tauri" / "Cargo.lock"
CARGO_SOURCES = GEN_DIR / "cargo-sources.json"
NODE_SOURCES = GEN_DIR / "node-sources.json"
NPM_LOCK = GEN_DIR / "package-lock.json"

# How many missing entries to name before truncating the report.
MAX_REPORTED = 15


def load_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        sys.exit(f"::error::missing generated manifest: {path.relative_to(REPO_ROOT)}")
    except json.JSONDecodeError as err:
        sys.exit(f"::error::{path.relative_to(REPO_ROOT)} is not valid JSON: {err}")


def expected_crates() -> set[str]:
    """`<name>-<version>.crate` for every crates.io package in the lock.

    Git and path dependencies are skipped: the generator emits those as
    `git` sources / the vendor dir reaches the sandbox through a `dir`
    source, so neither maps to a `.crate` tarball.
    """
    text = CARGO_LOCK.read_text(encoding="utf-8")
    wanted = set()
    for block in text.split("[[package]]"):
        name = re.search(r'^name = "(.+)"$', block, re.M)
        version = re.search(r'^version = "(.+)"$', block, re.M)
        registry = re.search(r'^source = "registry\+', block, re.M)
        if name and version and registry:
            wanted.add(f"{name.group(1)}-{version.group(1)}.crate")
    return wanted


def declared_crates(doc) -> set[str]:
    return {
        entry["url"].rsplit("/", 1)[-1]
        for entry in doc
        if isinstance(entry, dict) and str(entry.get("url", "")).endswith(".crate")
    }


def check_cargo() -> list[str]:
    wanted = expected_crates()
    if not wanted:
        return ["Cargo.lock parsed to zero registry crates — parser or lock format changed"]
    have = declared_crates(load_json(CARGO_SOURCES))
    missing = sorted(wanted - have)
    print(f"cargo: {len(wanted)} crates in Cargo.lock, {len(have)} declared as sources")
    if not missing:
        return []
    report = [f"{len(missing)} crate(s) in Cargo.lock have no source entry"]
    report += [f"    {name}" for name in missing[:MAX_REPORTED]]
    if len(missing) > MAX_REPORTED:
        report.append(f"    … and {len(missing) - MAX_REPORTED} more")
    return report


def check_node() -> list[str]:
    lock = load_json(NPM_LOCK)
    packages = lock.get("packages")
    if not packages:
        return ["package-lock.json has no `packages` map — lockfile version changed?"]
    have = {
        entry["url"]
        for entry in load_json(NODE_SOURCES)
        if isinstance(entry, dict) and "url" in entry
    }
    missing = sorted(
        resolved
        for pkg in packages.values()
        if (resolved := str(pkg.get("resolved", ""))).startswith("https://registry.npmjs.org/")
        and resolved not in have
    )
    print(f"node: {len(packages)} lockfile entries, {len(have)} tarballs declared as sources")
    if not missing:
        return []
    report = [f"{len(missing)} npm package(s) in the lockfile have no tarball source"]
    report += [f"    {url}" for url in missing[:MAX_REPORTED]]
    if len(missing) > MAX_REPORTED:
        report.append(f"    … and {len(missing) - MAX_REPORTED} more")
    return report


def main() -> int:
    problems = check_cargo() + check_node()
    if problems:
        print()
        for line in problems:
            print(f"::error::{line}" if not line.startswith("    ") else line)
        print(
            "\nThe generated Flatpak manifests are out of sync with the lockfiles.\n"
            "Regenerate them and commit the result:\n"
            "    bash packaging/flatpak/generate-sources.sh"
        )
        return 1
    print("\n✓ Flatpak sources cover both lockfiles.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
