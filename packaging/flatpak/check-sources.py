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
PACKAGE_JSON = REPO_ROOT / "package.json"
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


DEP_FIELDS = (
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
)


def check_manifest_sync() -> list[str]:
    """package.json's direct deps must match the Flatpak lockfile's root.

    `npm ci` refuses to run at all when the two disagree ("can only
    install packages when your package.json and package-lock.json are in
    sync"), so adding a dependency without regenerating breaks the
    Flathub build just as surely as an undeclared crate — and the tarball
    coverage check above can't see it, since the lockfile stays internally
    consistent with its own sources.
    """
    manifest = load_json(PACKAGE_JSON)
    root = load_json(NPM_LOCK).get("packages", {}).get("")
    if root is None:
        return ["package-lock.json has no root ('') package entry"]

    problems = []
    for field in DEP_FIELDS:
        want = manifest.get(field) or {}
        have = root.get(field) or {}
        if want == have:
            continue
        added = sorted(set(want) - set(have))
        removed = sorted(set(have) - set(want))
        changed = sorted(
            f"{n} ({have[n]} -> {want[n]})"
            for n in set(want) & set(have)
            if want[n] != have[n]
        )
        detail = "; ".join(
            part
            for part in (
                f"missing from lock: {', '.join(added)}" if added else "",
                f"stale in lock: {', '.join(removed)}" if removed else "",
                f"version spec changed: {', '.join(changed)}" if changed else "",
            )
            if part
        )
        problems.append(f"package.json {field} out of sync with the lockfile — {detail}")
    print(f"npm manifest: {len(DEP_FIELDS)} dependency fields compared against the lockfile root")
    return problems


def satisfies(version: str, spec: str) -> bool | None:
    """Minimal semver range check. `None` means "can't tell, don't fail".

    Covers only the forms an `overrides` entry realistically uses.
    Anything else — including any prerelease version, which npm excludes
    from ranges unless the range itself is a prerelease — comes back as
    None and is reported as unverified rather than failed: a false alarm
    here would train people to ignore this script.
    """
    if "-" in version:  # prerelease: npm's rules differ, don't guess
        return None
    parts = version.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        return None
    have = tuple(int(p) for p in parts)

    match = re.fullmatch(r"([~^]|>=|=)?\s*v?(\d+)\.(\d+)\.(\d+)", spec.strip())
    if not match:
        return None
    op = match.group(1)
    want = tuple(int(g) for g in match.groups()[1:])

    if op == ">=":
        return have >= want
    if op in (None, "="):
        return have == want

    # Upper bound, following npm's caret rules — which are NOT simply
    # "same major": below 1.0.0 the leftmost non-zero component is the
    # one pinned, because a 0.x bump is allowed to break.
    wm, wn, wp = want
    if op == "~":
        upper = (wm, wn + 1, 0)
    elif wm > 0:
        upper = (wm + 1, 0, 0)
    elif wn > 0:
        upper = (0, wn + 1, 0)
    else:
        upper = (0, 0, wp + 1)
    return want <= have < upper


def check_overrides() -> list[str]:
    """Every resolved copy of an overridden package must satisfy the pin.

    Checked against the resolved tree, not against a root `overrides`
    field: npm doesn't echo `overrides` into the lockfile, so there is
    nothing to compare it to. The pin only means something if the
    versions npm actually resolved obey it — which is the property the
    Flathub build depends on (the fast-uri advisory, #489).
    """
    overrides = load_json(PACKAGE_JSON).get("overrides") or {}
    if not overrides:
        return []
    packages = load_json(NPM_LOCK).get("packages", {})
    problems, checked, skipped = [], 0, []
    for name, spec in overrides.items():
        if not isinstance(spec, str):
            # Nested form ({".": "1", "sub": "2"}) — out of scope.
            skipped.append(f"{name} (nested override)")
            continue
        # Every copy, not one per name: npm nests a second copy under
        # `node_modules/<dep>/node_modules/<name>` when versions conflict,
        # and keying by name would let one silently replace the other —
        # so a nested, unpatched copy could slip through the very check
        # meant to catch it.
        resolved = {
            pkg["version"]
            for path, pkg in packages.items()
            if path.rsplit("node_modules/", 1)[-1] == name and pkg.get("version")
        }
        if not resolved:
            # Overriding something the tree no longer pulls in is dead
            # weight, not a build failure — say so without failing.
            skipped.append(f"{name} (not in the tree)")
            continue
        for version in sorted(resolved):
            verdict = satisfies(version, spec)
            if verdict is None:
                skipped.append(f"{name} (unhandled spec {spec!r})")
            elif not verdict:
                problems.append(
                    f"override {name}@{spec} not honoured — lockfile resolves {version}"
                )
            else:
                checked += 1
    print(f"npm overrides: {checked} resolution(s) verified" + (f", skipped: {'; '.join(skipped)}" if skipped else ""))
    return problems


def self_test() -> int:
    """Exercise `satisfies` — the only non-obvious logic in here.

    Each caret case pins the boundary *and* the first version past it,
    because the bug worth catching is an over-permissive upper bound
    (silently accepting a version npm would have rejected).
    """
    cases: list[tuple[str, str, bool | None]] = [
        # caret, major >= 1 — pinned to the major
        ("3.1.5", "^3.1.5", True),
        ("3.9.0", "^3.1.5", True),
        ("3.1.4", "^3.1.5", False),
        ("4.0.0", "^3.1.5", False),
        # caret, 0.x — pinned to the minor
        ("0.1.2", "^0.1.2", True),
        ("0.1.99", "^0.1.2", True),
        ("0.1.1", "^0.1.2", False),
        ("0.2.0", "^0.1.2", False),
        # caret, 0.0.x — pinned to the exact patch
        ("0.0.3", "^0.0.3", True),
        ("0.0.4", "^0.0.3", False),
        ("0.1.0", "^0.0.3", False),
        # tilde — pinned to the minor whatever the major
        ("1.2.9", "~1.2.3", True),
        ("1.3.0", "~1.2.3", False),
        ("1.2.2", "~1.2.3", False),
        # comparators + exact
        ("9.9.9", ">=1.0.0", True),
        ("0.9.9", ">=1.0.0", False),
        ("2.0.0", "2.0.0", True),
        ("2.0.1", "2.0.0", False),
        # unverifiable: prerelease version, or a range form we don't parse
        ("3.1.5-rc.1", "^3.1.5", None),
        ("3.1.5", "3.x", None),
        ("3.1.5", ">1.0.0 <4.0.0", None),
    ]
    failures = 0
    for version, spec, expected in cases:
        got = satisfies(version, spec)
        if got is not expected:
            failures += 1
            print(f"::error::satisfies({version!r}, {spec!r}) = {got!r}, expected {expected!r}")
    print(f"self-test: {len(cases) - failures}/{len(cases)} semver cases passed")
    return 1 if failures else 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()
    problems = check_cargo() + check_node() + check_manifest_sync() + check_overrides()
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
