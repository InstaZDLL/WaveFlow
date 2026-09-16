#!/usr/bin/env python3
"""Verify our crate and `windows` agree on which `windows-core` they mean.

`crates/app` names `windows-core` itself, and it has to: `#[implement]`
— which builds the `IMMNotificationClient` sink of #627 — expands to
`::windows_core::` paths, so the crate must be nameable from our own
root. Taking it through `windows::core` is not an option, because the
macro writes the path.

That makes the pair an invariant rather than a preference. If our
`windows-core` resolves to a different copy than the one `windows` was
built against, `#[implement]` generates an `IUnknownImpl` from one copy
while `windows` expects the other, and the sink no longer satisfies the
interface. The failure is a wall of trait errors naming neither
version, on the one platform CI compiles but nobody reads the lockfile
for. #642 broke exactly that way.

`.github/dependabot.yml` holds both names back from unpaired bumps, but
that only covers the updates Dependabot proposes. Every other route
into `Cargo.lock` — a manual `cargo update`, a transitive bump that
lets a different `windows` win the unification, a lockfile conflict
resolved by hand — is unguarded. This is that guard.

## Why this does not compare version numbers

The obvious check — read the locked `windows` and `windows-core`, fail
if the strings differ — is wrong, and this lockfile already proves it:
`windows 0.61.3` depends on `windows-core 0.61.2`. The two lines are
numbered independently upstream, and the gap is widening rather than
closing: windows-rs 0.100.0 renumbered the whole support layer —
`windows-core`, `-implement`, `-interface`, `-result`, `-link` — while
the generated-bindings crates above it stayed on 0.62. Equal strings
are neither necessary nor sufficient, and soon enough they will not
even be common.

What matters is identity: the `windows-core` our crate names must be
the same node in the graph as the `windows-core` under the `windows`
our crate names. That is what Cargo unification is being asked for, and
it is what this walks. Identity, not equality — the comparison below is
`is`, between two packages read out of one lockfile.

The name alone does not identify a node either. Three `windows` and two
`windows-core` are locked here — Tauri's GTK stack and `wasapi` bring
their own — so "the version of `windows` in Cargo.lock" is not a
question the file answers. Nor does name plus version: a `[patch]` or a
`git` dependency puts two packages of the same name *and* version in
the graph, told apart only by `source`, and Cargo writes the source
into the edge precisely when it has to. Every lookup below resolves the
edge as Cargo wrote it, against the whole package list.

Runs offline in well under a second: `tomllib` is stdlib since 3.11,
and `Cargo.lock` is TOML.
"""

from __future__ import annotations

import re
import sys

# Checked before the import rather than left to `ModuleNotFoundError`,
# which names `tomllib` and not the reason. CI's `ubuntu-latest` ships
# 3.12; a contributor running this by hand, as CONTRIBUTING asks, may
# not.
if sys.version_info < (3, 11):
    sys.exit(
        "check-windows-core-pair.py needs Python 3.11+ for tomllib "
        f"(running {sys.version_info.major}.{sys.version_info.minor})"
    )

import tomllib  # noqa: E402  (deliberately after the version guard)
from pathlib import Path  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
LOCKFILE = ROOT / "src-tauri" / "Cargo.lock"

WINDOWS = "windows"
CORE = "windows-core"

# `name`, `name version`, or `name version (source)` — Cargo writes the
# shortest form that is unambiguous, so all three have to be read.
EDGE_RE = re.compile(r"^(?P<name>\S+)(?: (?P<version>[^\s(]+))?(?: \((?P<source>.*)\))?$")

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def label(package: dict) -> str:
    """`name version`, plus the source when it is what distinguishes it."""
    source = package.get("source")
    name = f"{package['name']} {package['version']}"
    return name if source in (None, CRATES_IO) else f"{name} ({source})"


class Graph:
    """The locked packages, addressed the way dependency edges address them."""

    def __init__(self, packages: list[dict]):
        self.packages = packages

    @classmethod
    def parse(cls, text: str) -> "Graph":
        return cls(tomllib.loads(text).get("package", []))

    def resolve(self, package: dict, name: str) -> tuple[dict | None, str | None]:
        """`(package, error)` for `package`'s direct edge to `name`.

        Returns `(None, None)` when there is no such edge — not every
        caller requires one.

        An edge is matched against the whole package list on every field
        it carries. Filling in a field it omits would be a guess, and a
        guess here is how a check reports "aligned" about a package it
        never looked at.
        """
        found: list[dict] = []

        for entry in package.get("dependencies", []):
            parsed = EDGE_RE.match(entry.strip())
            if not parsed or parsed["name"] != name:
                continue

            matches = [
                candidate
                for candidate in self.packages
                if candidate["name"] == name
                and parsed["version"] in (None, candidate["version"])
                and parsed["source"] in (None, candidate.get("source"))
            ]
            if len(matches) != 1:
                return None, (
                    f"`{label(package)}` depends on `{entry}`, which matches "
                    f"{len(matches)} locked packages"
                )
            found.append(matches[0])

        if not found:
            return None, None
        if any(other is not found[0] for other in found):
            return None, (
                f"`{label(package)}` depends on `{name}` more than once, "
                f"on different copies ({', '.join(sorted({label(f) for f in found}))})"
            )
        return found[0], None

    def local_packages(self) -> list[dict]:
        """The packages this repository can edit.

        A package with no `source` is one Cargo read off disk: the five
        workspace members, and the vendored `glib` the root `[patch]`
        points at. That is the right scope rather than an accident of
        the field — a registry crate whose own pair is split is not
        something a pull request here can fix, and failing on it would
        only teach people to ignore this check.
        """
        return [package for package in self.packages if "source" not in package]


def problems(graph: Graph) -> list[str]:
    found: list[str] = []

    for member in graph.local_packages():
        named_core, error = graph.resolve(member, CORE)
        if error:
            found.append(error)
            continue
        if named_core is None:
            # The invariant only binds a crate that names `windows-core`
            # itself. One that reaches it through `windows` has nothing
            # of its own to keep in step.
            continue

        windows, error = graph.resolve(member, WINDOWS)
        if error:
            found.append(error)
            continue
        if windows is None:
            found.append(
                f"`{member['name']}` names `{label(named_core)}` but not `{WINDOWS}` -- "
                f"the dependency exists to match `{WINDOWS}`'s own copy, so alone it is "
                f"either dead weight or a missing dependency"
            )
            continue

        core_under_windows, error = graph.resolve(windows, CORE)
        if error:
            found.append(error)
            continue
        if core_under_windows is None:
            found.append(
                f"`{label(windows)}` does not depend on `{CORE}` at all, so "
                f"`{member['name']}`'s `{label(named_core)}` matches nothing -- "
                f"`{WINDOWS}` only grew a `core` module in 0.48"
            )
            continue

        if core_under_windows is not named_core:
            found.append(
                f"`{member['name']}` resolves `{label(named_core)}`, but the "
                f"`{label(windows)}` it depends on was built against "
                f"`{label(core_under_windows)}`. `#[implement]` would expand to one "
                f"crate's `IUnknownImpl` while `{WINDOWS}` expects the other's, and no "
                f"COM sink in the crate would satisfy its interface"
            )

    return found


def checked_pairs(graph: Graph) -> list[str]:
    """`crate -> windows-core` for every local crate that names it.

    Its emptiness is the answer to "did this check guard anything?",
    which is why [`main`] treats an empty list as a failure rather than
    a clean bill of health.
    """
    listed = []
    for member in graph.local_packages():
        core, _ = graph.resolve(member, CORE)
        if core is not None:
            listed.append(f"{member['name']} -> {label(core)}")
    return listed


# --------------------------------------------------------------------
# Self-test
# --------------------------------------------------------------------

ALIGNED = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows 0.62.2",
 "windows-core 0.62.2",
]

[[package]]
name = "windows"
version = "0.61.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.61.2",
]

[[package]]
name = "windows"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.62.2",
]

[[package]]
name = "windows-core"
version = "0.61.2"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "windows-core"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# The same graph with our two edges pulled apart: `windows-core` slips
# back to the copy Tauri's stack pulls in, while `windows` stays put.
# Note what a check that read "the version of windows" by name would
# see here — two candidates, and nothing in the file to choose between
# them.
SPLIT = ALIGNED.replace(' "windows-core 0.62.2",\n]', ' "windows-core 0.61.2",\n]', 1)

# Equal numbers, different copies: `windows 0.61.3` is built against
# `windows-core 0.61.2`, so naming 0.61.3 of the latter is a real break
# that a string comparison calls a match.
EQUAL_NUMBERS_STILL_WRONG = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows 0.61.3",
 "windows-core 0.61.3",
]

[[package]]
name = "windows"
version = "0.61.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.61.2",
]

[[package]]
name = "windows-core"
version = "0.61.2"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "windows-core"
version = "0.61.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# Differing numbers, one copy — a shape windows-rs has actually
# shipped, and the one a string comparison rejects out of hand.
UNEQUAL_NUMBERS_STILL_RIGHT = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows 0.61.3",
 "windows-core 0.61.2",
]

[[package]]
name = "windows"
version = "0.61.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.61.2",
]

[[package]]
name = "windows-core"
version = "0.61.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# Same name, same version, two copies — a `[patch]` or a `git`
# dependency, which this workspace already uses for glib. Cargo writes
# the source into the edge exactly here, and a key of (name, version)
# collapses the two into whichever was read last.
SAME_VERSION_TWO_SOURCES = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows 0.62.2",
 "windows-core 0.62.2 (git+https://github.com/microsoft/windows-rs)",
]

[[package]]
name = "windows"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.62.2 (registry+https://github.com/rust-lang/crates.io-index)",
]

[[package]]
name = "windows-core"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "windows-core"
version = "0.62.2"
source = "git+https://github.com/microsoft/windows-rs"
"""

# The same two copies, both edges on the patched one. Nothing wrong
# with that, and a check that reported the mere presence of two copies
# would be crying wolf at a deliberate `[patch]`.
TWO_SOURCES_BUT_AGREED = SAME_VERSION_TWO_SOURCES.replace(
    ' "windows-core 0.62.2 (registry+https://github.com/rust-lang/crates.io-index)",',
    ' "windows-core 0.62.2 (git+https://github.com/microsoft/windows-rs)",',
    1,
)

# One version of each: Cargo writes the edges without versions, and a
# resolver that only understands the two-word form sees no edge at all
# — then reports a clean graph it never examined.
BARE_NAMES = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows",
 "windows-core",
]

[[package]]
name = "windows"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core",
]

[[package]]
name = "windows-core"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# A bare name Cargo would never write, because two are locked. Reading
# it as "the only one" is the guess this has to refuse.
BARE_NAME_BUT_AMBIGUOUS = ALIGNED.replace(
    ' "windows-core 0.62.2",\n]', ' "windows-core",\n]', 1
)

# Nothing to keep in step: no `windows-core` edge of its own.
NOT_CONCERNED = """
[[package]]
name = "waveflow-core"
version = "1.7.0"
dependencies = [
 "windows 0.62.2",
]

[[package]]
name = "windows"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "windows-core 0.62.2",
]

[[package]]
name = "windows-core"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# `windows` from before it had a `core` module. Nothing for our edge to
# match, and the walk has to say so rather than dereference `None`.
NO_CORE_UNDER_WINDOWS = """
[[package]]
name = "waveflow"
version = "1.7.0"
dependencies = [
 "windows 0.44.0",
 "windows-core 0.62.2",
]

[[package]]
name = "windows"
version = "0.44.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "windows-core"
version = "0.62.2"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

# A registry package whose own pair is split. Not ours to fix, so not
# ours to fail on: only the crates we can edit are checked.
THEIR_PROBLEM = SPLIT.replace(
    'name = "waveflow"\nversion = "1.7.0"',
    'name = "someone-else"\nversion = "1.0.0"\n'
    'source = "registry+https://github.com/rust-lang/crates.io-index"',
)


def self_test() -> int:
    """Pin down the graph walk. Every case below is a way to be wrong.

    Three of them pass a version-string comparison — the check this
    file deliberately is not. Two pass a lookup by package name, which
    is what "read the version from Cargo.lock" means once three of that
    name are locked. One passes a lookup by name *and* version, which
    is the same mistake one field further in.
    """
    failures: list[str] = []

    def count(label: str, lock: str, wanted: int, expect_in: str = ""):
        found = problems(Graph.parse(lock))
        if len(found) != wanted:
            failures.append(f"{label}: got {len(found)} problem(s) {found}, wanted {wanted}")
        elif expect_in and not any(expect_in in message for message in found):
            failures.append(f"{label}: no message containing {expect_in!r} in {found}")

    count("the real shape is accepted", ALIGNED, 0)
    count("a split pair is caught", SPLIT, 1, "0.61.2")
    count(
        "equal numbers are not the invariant",
        EQUAL_NUMBERS_STILL_WRONG,
        1,
        "built against `windows-core 0.61.2`",
    )
    count("unequal numbers are not a failure", UNEQUAL_NUMBERS_STILL_RIGHT, 0)
    count(
        "one version from two sources is two copies",
        SAME_VERSION_TWO_SOURCES,
        1,
        "git+https://github.com/microsoft/windows-rs",
    )
    count("two copies both edges agree on are fine", TWO_SOURCES_BUT_AGREED, 0)
    count("bare names resolve when unambiguous", BARE_NAMES, 0)
    count("a bare ambiguous name is refused", BARE_NAME_BUT_AMBIGUOUS, 1, "matches 2 locked")
    count("a crate that does not name windows-core is skipped", NOT_CONCERNED, 0)
    count("windows without a core module is reported", NO_CORE_UNDER_WINDOWS, 1, "0.48")
    count("a registry package's own split is not ours", THEIR_PROBLEM, 0)

    # An empty listing is what `main` refuses to call success, so it has
    # to actually be empty in the shape that means it.
    if checked_pairs(Graph.parse(ALIGNED)) != ["waveflow -> windows-core 0.62.2"]:
        failures.append(f"the real shape lists nothing: {checked_pairs(Graph.parse(ALIGNED))}")
    if checked_pairs(Graph.parse(NOT_CONCERNED)) != []:
        failures.append("a crate that does not name windows-core was listed as checked")

    # The failing message has to carry both observed copies, or it sends
    # the reader back to the lockfile to work out what broke.
    message = problems(Graph.parse(SPLIT))[0]
    for version in ("0.61.2", "0.62.2"):
        if version not in message:
            failures.append(f"the failure message omits {version}: {message}")

    if failures:
        print("self-test failed:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print("self-test: 15 assertions passed")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()

    if not LOCKFILE.exists():
        sys.exit(f"missing {LOCKFILE.relative_to(ROOT)}")

    graph = Graph.parse(LOCKFILE.read_text(encoding="utf-8"))
    found = problems(graph)

    if found:
        print("windows and windows-core are not the pair they have to be:\n", file=sys.stderr)
        for problem in found:
            print(f"  {problem}", file=sys.stderr)
        print(
            "\nBump both lines of the Windows target dependencies in "
            "src-tauri/crates/app/Cargo.toml together, to whatever windows-rs released "
            "as a set, and commit the regenerated Cargo.lock.",
            file=sys.stderr,
        )
        return 1

    # Name what was actually checked, and refuse to report success on an
    # empty list. "No pair to check" and "the pair is fine" print the
    # same word otherwise, and the first is the state this whole file
    # exists to notice.
    checked = checked_pairs(graph)
    if not checked:
        for line in (
            f"no local crate names `{CORE}`, so this checked nothing:",
            "",
            "  Either the dependency is gone -- then `#[implement]` has no path to",
            "  expand to, and this script should go in the same commit -- or it is",
            f"  still declared and {LOCKFILE.name} is no longer being read correctly.",
        ):
            print(line, file=sys.stderr)
        return 1

    print(f"windows-core pair: {', '.join(checked)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
