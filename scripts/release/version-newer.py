#!/usr/bin/env python3
"""Tell whether one release version is newer than another.

    version-newer.py <candidate> <current>

Exits 0 when <candidate> is newer, 1 when it is not, and 2 when either
is not a version — so a caller cannot read a failure as "not newer",
which for a guard that keeps a channel from moving backwards would mean
"go ahead". `release.yml` and `publish-r2.sh` both ask it, so the two
answers cannot drift apart.

Semver precedence: a leading `v` and build metadata (`+…`) are ignored,
a release sorts after every pre-release of the same core, and
pre-release identifiers compare numerically where they can.

`--self-test` checks the ordering and the refusals.
"""

from __future__ import annotations

import sys


class NotAVersion(ValueError):
    pass


def key(version: str):
    text = version.strip()
    if text.startswith("v"):
        text = text[1:]
    text = text.split("+", 1)[0]
    core, dash, pre = text.partition("-")
    numbers = core.split(".")
    if not numbers or not all(part.isdigit() for part in numbers):
        raise NotAVersion(f"{version!r} is not a version")
    parts = pre.split(".") if dash else []
    if any(not part for part in parts):
        raise NotAVersion(f"{version!r} has an empty pre-release identifier")
    return (
        tuple(int(part) for part in numbers),
        # A release sorts after every pre-release of the same core.
        (1,) if not parts else (0,) + tuple(
            (0, int(p), "") if p.isdigit() else (1, 0, p) for p in parts
        ),
    )


def newer(candidate: str, current: str) -> bool:
    return key(candidate) > key(current)


def self_test() -> int:
    failures = 0

    def expect(label: str, ok: bool) -> None:
        nonlocal failures
        print(("ok   " if ok else "FAIL ") + label)
        failures += not ok

    expect("a release outranks its own betas", newer("1.8.0", "1.8.0-beta.3"))
    expect("a beta does not outrank its release", not newer("1.8.0-beta.3", "1.8.0"))
    expect("beta.10 is newer than beta.3", newer("1.8.0-beta.10", "1.8.0-beta.3"))
    expect("an older stable does not outrank a newer beta", not newer("1.8.1", "1.9.0-beta.1"))
    expect("the same version is not newer", not newer("2.0.1", "2.0.1"))
    expect("a leading v is ignored", not newer("v2.0.1", "2.0.1") and newer("v2.0.2", "2.0.1"))
    expect("build metadata is ignored", not newer("2.0.1+r2", "2.0.1+gh"))
    expect("2.0.10 is newer than 2.0.9", newer("2.0.10", "2.0.9"))
    for bad in ("abc", "", "2.0.x", "2.0.1-", "2.0.1-beta..1"):
        try:
            key(bad)
        except NotAVersion:
            expect(f"{bad!r} refused", True)
        else:
            expect(f"{bad!r} refused", False)
    print("self-test", "failed" if failures else "passed")
    return 1 if failures else 0


def main(argv: list[str]) -> int:
    if argv[1:] == ["--self-test"]:
        return self_test()
    if len(argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        return 0 if newer(argv[1], argv[2]) else 1
    except NotAVersion as err:
        print(f"::error::cannot compare versions: {err}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
