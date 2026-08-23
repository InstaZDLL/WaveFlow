#!/usr/bin/env python3
"""Pick the Actions caches that a newer cache has made unreachable.

`Swatinem/rust-cache` v2 matches its key exactly and deliberately ships
no `restore-keys` fallback, because a partially stale Rust cache is
often worse than none. The key ends in a hash of the workspace
manifests, so every merge to `main` that touches a `Cargo.toml`, the
lockfile or `rust-toolchain.toml` mints a whole new generation — about
2.8 GB across the Linux, Windows and CodeQL entries — and leaves the
previous one in place.

Nothing will ever ask for that previous one again: the only key a
future run derives is the one matching the current tree. It is dead
weight against a 10 GB repository quota, and the repository sat at
9.6 GB with three live generations when this was written (#535).

Age cannot spot them. The workflow's older rule dropped caches on
`main` unread for seven days, and the three generations had all been
read within the hour — every pull request restores the newest one,
which says nothing about the two it supersedes.

What identifies a dead cache is structural: another cache exists whose
key shares its prefix and whose creation is more recent. So this script
groups by key-minus-the-trailing-hashes, keeps the newest of each
group, and names the rest.

Reads `gh cache list --json id,key,createdAt,sizeInBytes` on stdin,
writes the ids to delete on stdout one per line, and a readable summary
on stderr. It deletes nothing and makes no network call, which is what
lets `--self-test` cover the whole decision.
"""

import argparse
import json
import re
import sys

# Cache keys end in one or more hex digests: rust-cache appends an
# environment hash and a lockfile hash (`-718c915e-6da14145`), while the
# `actions/cache` steps here append a single long one. Six digits is the
# shortest we mint, and requiring hex keeps real words out — the
# segments that must survive stripping (`x64`, `Linux`, `test-appimage`,
# `release`) all carry letters past `f`.
HASH_SUFFIX = re.compile(r"-[0-9a-f]{6,}$")


def group_of(key):
    """The stable part of a cache key: everything before its hashes."""
    while True:
        stripped = HASH_SUFFIX.sub("", key)
        if stripped == key:
            return key
        key = stripped


def superseded(caches, keep=1):
    """Caches outranked by `keep` newer entries sharing their group.

    Ordering is by creation, not by list order: `gh cache list` sorts by
    last access by default, and last access is precisely the signal that
    fails to tell a live generation from a dead one.
    """
    groups = {}
    for cache in caches:
        groups.setdefault(group_of(cache["key"]), []).append(cache)

    doomed = []
    for group in groups.values():
        group.sort(key=lambda c: (c["createdAt"], c["id"]), reverse=True)
        doomed.extend(group[keep:])
    return doomed


def megabytes(size):
    return size / (1024 * 1024)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Name the superseded Actions caches on stdin.")
    parser.add_argument(
        "--keep",
        type=int,
        default=1,
        metavar="N",
        help="generations to keep per key group (default: 1)",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the built-in tests and exit",
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    if args.keep < 1:
        parser.error("--keep must be at least 1; keeping none deletes the entry every run restores")

    caches = json.load(sys.stdin)
    doomed = superseded(caches, keep=args.keep)

    if not doomed:
        print("No superseded caches: every key group is a single generation.", file=sys.stderr)
        return 0

    freed = sum(c.get("sizeInBytes", 0) for c in doomed)
    print(f"Superseded caches: {len(doomed)}, {megabytes(freed):.0f} MB", file=sys.stderr)
    for cache in doomed:
        size = megabytes(cache.get("sizeInBytes", 0))
        print(f"  {cache['key']}  ({size:.0f} MB, created {cache['createdAt']})", file=sys.stderr)
        print(cache["id"])
    return 0


def self_test():
    """Cover the decision on hand-written listings.

    The workflow around this script deletes what it prints, so a
    grouping bug is destructive rather than merely wrong. This runs in
    CI before any real listing is ever piped in.
    """

    def cache(id, key, created, size=1):
        return {"id": id, "key": key, "createdAt": created, "sizeInBytes": size}

    failures = []

    def check(label, got, want):
        if got != want:
            failures.append(f"{label}: expected {want}, got {got}")

    def ids(caches, **kwargs):
        return sorted(c["id"] for c in superseded(caches, **kwargs))

    # The case that motivated the script: three rust-cache generations
    # on main, differing only in the lockfile hash.
    generations = [
        cache(1, "v0-rust-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:40:34Z"),
        cache(2, "v0-rust-rust-Linux-x64-718c915e-f884c40f", "2026-08-22T23:58:59Z"),
        cache(3, "v0-rust-rust-Linux-x64-718c915e-6da14145", "2026-08-23T07:37:57Z"),
    ]
    check("keeps only the newest generation", ids(generations), [1, 2])
    check("keep=2 spares the runner-up", ids(generations, keep=2), [1])
    check("keep=3 spares them all", ids(generations, keep=3), [])

    # An environment hash bump (a compiler upgrade) strands the old
    # generation just as completely, so both hashes must be stripped.
    check(
        "a changed environment hash also supersedes",
        ids(
            [
                cache(1, "v0-rust-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:40:34Z"),
                cache(2, "v0-rust-rust-Linux-x64-99999999-2e9a1d04", "2026-08-23T07:37:57Z"),
            ]
        ),
        [1],
    )

    # Different jobs must never pool: deleting across them would drop a
    # cache that no newer entry replaces.
    check(
        "operating systems and jobs stay separate",
        ids(
            [
                cache(1, "v0-rust-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:40:34Z"),
                cache(2, "v0-rust-rust-Windows_NT-x64-e61e1838-2e9a1d04", "2026-08-22T23:35:33Z"),
                cache(3, "v0-rust-analyze-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:31:16Z"),
            ]
        ),
        [],
    )

    # `linux-test-appimage` and `macos-release` end in words, and the
    # words must survive: strip them and unrelated jobs would merge.
    check(
        "word-suffixed job keys are not treated as hashes",
        sorted(
            group_of(k)
            for k in [
                "v0-rust-linux-test-appimage-Linux-x64-718c915e-2e9a1d04",
                "v0-rust-macos-release-macOS-arm64-718c915e-2e9a1d04",
                "v0-rust-release-please-lockfile-build-Linux-x64-718c915e-6da14145",
            ]
        ),
        [
            "v0-rust-linux-test-appimage-Linux-x64",
            "v0-rust-macos-release-macOS-arm64",
            "v0-rust-release-please-lockfile-build-Linux-x64",
        ],
    )

    # The bun cache uses one long digest rather than two short ones.
    check(
        "a single long digest is stripped too",
        group_of("bun-Linux-6623d2a32473f344ba794dcb2ddaa0e88746caa893a28486d9ecdb7361b1d961"),
        "bun-Linux",
    )

    # `gh cache list` sorts by last access, so the newest generation can
    # arrive anywhere in the listing.
    check(
        "input order does not decide what survives",
        ids(
            [
                cache(3, "v0-rust-rust-Linux-x64-718c915e-6da14145", "2026-08-23T07:37:57Z"),
                cache(1, "v0-rust-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:40:34Z"),
                cache(2, "v0-rust-rust-Linux-x64-718c915e-f884c40f", "2026-08-22T23:58:59Z"),
            ]
        ),
        [1, 2],
    )

    check("an empty listing is not an error", ids([]), [])
    check(
        "a lone cache is never superseded",
        ids([cache(1, "v0-rust-rust-Linux-x64-718c915e-2e9a1d04", "2026-08-22T23:40:34Z")]),
        [],
    )

    # A key carrying no hash at all still groups as itself rather than
    # collapsing into a neighbour.
    check("an unhashed key groups as itself", group_of("plain-key"), "plain-key")

    for failure in failures:
        print(f"FAIL {failure}", file=sys.stderr)
    if failures:
        print(f"{len(failures)} self-test failure(s)", file=sys.stderr)
        return 1
    print("Self-test passed.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
