#!/usr/bin/env python3
"""Verify every workflow installs the compiler `rust-toolchain.toml` names.

`rust-toolchain.toml` is read by rustup, and the workflows deliberately
do not read it: rustup would install `stable` first and then download
the pinned toolchain again on the first cargo call. The version is
therefore written in six places, and nothing made them agree.

That is a silent failure, which is the only kind worth a script. Bump
the file, forget `release.yml`, and CI stays green while the release is
built by a compiler nobody chose — with the formatter and clippy, the
two things the pin exists to make deterministic, quietly answering
differently depending on which job asked.

Checks two directions:

- every `toolchain:` input across `.github/workflows/` equals the
  pinned channel;
- every *step* that installs Rust passes one of its own, so a step
  cannot drift back to an implicit `stable`.

The second is per-step on purpose. Asking only whether the file
contains a `toolchain:` somewhere would let a workflow with two Rust
steps pass while one of them pins nothing, and would let an unrelated
`toolchain:` input belonging to another action stand in for the missing
one.

The equality check stays file-wide, which is the conservative
direction: an unrelated `toolchain:` would be flagged rather than
ignored, and the exemption marker is how you say so.

A line ending in `# toolchain-pin: exempt` opts out, for the day a
workflow genuinely needs a different compiler. It has to *end* the
line — a marker buried mid-line is a typo, not a decision.

Runs offline in well under a second: no network, no YAML dependency.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOOLCHAIN_FILE = ROOT / "rust-toolchain.toml"
WORKFLOW_DIR = ROOT / ".github" / "workflows"

CHANNEL_RE = re.compile(r'^\s*channel\s*=\s*"([^"]+)"', re.MULTILINE)
TOOLCHAIN_INPUT_RE = re.compile(r"^\s*toolchain:\s*(\S+)")
INSTALLS_RUST_RE = re.compile(r"dtolnay/rust-toolchain|actions-rs/toolchain")
LIST_ITEM_RE = re.compile(r"^(\s*)-\s")
EXEMPT = "# toolchain-pin: exempt"


def pinned_channel() -> str:
    if not TOOLCHAIN_FILE.exists():
        sys.exit(f"missing {TOOLCHAIN_FILE.relative_to(ROOT)}")
    match = CHANNEL_RE.search(TOOLCHAIN_FILE.read_text(encoding="utf-8"))
    if not match:
        sys.exit(f'no `channel = "..."` in {TOOLCHAIN_FILE.relative_to(ROOT)}')
    return match.group(1)


def is_exempt(line: str) -> bool:
    """The marker has to end the line. Anywhere else it is a typo."""
    return line.rstrip().endswith(EXEMPT)


def steps(text: str):
    """Yield `(line_number, block)` for each step of every `steps:` list.

    Anchored to the `steps:` key rather than to list items in general.
    An earlier, shallower list — `on: schedule:` and its `- cron:` is
    the usual one — would otherwise fix the item indentation for the
    whole file, and every step of every job would land in one block. A
    `toolchain:` from one step would then satisfy another, which is
    precisely the hole this function exists to close.

    Still a line scanner, deliberately: importing a YAML parser to read
    five workflow files would trade a dependency for a check that has to
    keep working on a bare runner.
    """
    lines = text.splitlines()
    index = 0

    while index < len(lines):
        header = re.match(r"^(\s*)steps:\s*(#.*)?$", lines[index])
        if not header:
            index += 1
            continue

        key_indent = len(header.group(1))
        index += 1
        item_indent = None
        block: list[str] = []
        start = 0

        while index < len(lines):
            line = lines[index]
            stripped = line.strip()
            if stripped and not stripped.startswith("#"):
                if len(line) - len(line.lstrip()) <= key_indent:
                    break

            item = LIST_ITEM_RE.match(line)
            if item and (item_indent is None or len(item.group(1)) == item_indent):
                if block:
                    yield start, "\n".join(block)
                item_indent = len(item.group(1))
                block, start = [line], index + 1
            elif block:
                block.append(line)
            index += 1

        if block:
            yield start, "\n".join(block)


def unpinned_rust_steps(text: str) -> list[int]:
    """Line numbers of steps that install Rust without pinning it."""
    offenders = []
    for number, block in steps(text):
        if not INSTALLS_RUST_RE.search(block):
            continue
        if any(TOOLCHAIN_INPUT_RE.match(line) for line in block.splitlines()):
            continue
        offenders.append(number)
    return offenders


def mismatched_inputs(text: str, channel: str) -> list[tuple[int, str]]:
    """`(line, value)` for every non-exempt `toolchain:` off the pin."""
    wrong = []
    for number, line in enumerate(text.splitlines(), start=1):
        match = TOOLCHAIN_INPUT_RE.match(line)
        if not match or is_exempt(line):
            continue
        value = match.group(1).strip("\"'")
        if value != channel:
            wrong.append((number, value))
    return wrong


def count_inputs(text: str) -> int:
    return sum(
        1
        for line in text.splitlines()
        if TOOLCHAIN_INPUT_RE.match(line) and not is_exempt(line)
    )


SCHEDULE_BEFORE_JOBS = """\
name: Fixture
on:
  schedule:
    - cron: "0 3 * * 1"
jobs:
  build:
    steps:
      - uses: dtolnay/rust-toolchain@sha
        with:
          toolchain: 1.98.0
      - uses: dtolnay/rust-toolchain@sha
        with:
          components: clippy
"""

TWO_JOBS = """\
jobs:
  one:
    steps:
      - uses: dtolnay/rust-toolchain@sha
        with:
          toolchain: 1.98.0
  two:
    steps:
      - uses: dtolnay/rust-toolchain@sha
"""

UNRELATED_INPUT = """\
jobs:
  build:
    steps:
      - uses: some/other-action@v1
        with:
          toolchain: 1.98.0
      - uses: dtolnay/rust-toolchain@sha
        with:
          components: clippy
"""

PINNED = """\
jobs:
  build:
    steps:
      - uses: dtolnay/rust-toolchain@sha
        with:
          toolchain: 1.98.0
"""


def self_test() -> int:
    """Pin down the parsing. Both mistakes below were shipped once.

    The step splitter matched any list item, then took its indentation
    from whichever list came first in the file. Each time the check kept
    passing something it exists to catch, which is the failure mode a
    guard cannot afford.
    """
    failures: list[str] = []

    def expect(label: str, actual, wanted):
        if actual != wanted:
            failures.append(f"{label}: got {actual!r}, wanted {wanted!r}")

    expect(
        "a shallower list before jobs must not merge the steps",
        unpinned_rust_steps(SCHEDULE_BEFORE_JOBS),
        [11],
    )
    expect(
        "a step in another job must not satisfy this one",
        unpinned_rust_steps(TWO_JOBS),
        [9],
    )
    expect(
        "an unrelated toolchain: input is not a pin",
        unpinned_rust_steps(UNRELATED_INPUT),
        [7],
    )
    expect("a pinned step is accepted", unpinned_rust_steps(PINNED), [])

    expect("marker at end of line exempts", is_exempt(f"  toolchain: nightly {EXEMPT}"), True)
    expect("marker mid-line does not", is_exempt(f"  toolchain: nightly {EXEMPT} # note"), False)
    expect("no marker", is_exempt("  toolchain: 1.98.0"), False)

    expect(
        "an off-pin value is reported with its line",
        mismatched_inputs("      toolchain: 1.97.0\n", "1.98.0"),
        [(1, "1.97.0")],
    )
    expect(
        "an exempt value is not compared",
        mismatched_inputs(f"      toolchain: nightly {EXEMPT}\n", "1.98.0"),
        [],
    )

    if failures:
        print("self-test failed:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1

    print("self-test: 9 assertions passed")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()

    channel = pinned_channel()
    problems: list[str] = []
    checked = 0

    workflows = sorted(WORKFLOW_DIR.glob("*.yml")) + sorted(WORKFLOW_DIR.glob("*.yaml"))
    for workflow in workflows:
        rel = workflow.relative_to(ROOT).as_posix()
        text = workflow.read_text(encoding="utf-8")
        checked += count_inputs(text)

        for number, value in mismatched_inputs(text, channel):
            problems.append(
                f"{rel}:{number}: installs {value}, but rust-toolchain.toml pins {channel}"
            )
        for number in unpinned_rust_steps(text):
            problems.append(
                f"{rel}:{number}: installs Rust without a `toolchain:` input of its own, "
                f"so it resolves to whatever the action defaults to instead of {channel}"
            )

    if problems:
        print("Toolchain pin is not consistent:\n", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        print(
            f"\nBump every site together, or mark a deliberate exception with "
            f"`{EXEMPT}` at the end of the line.",
            file=sys.stderr,
        )
        return 1

    print(f"toolchain pin: {checked} workflow input(s) all on {channel}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
