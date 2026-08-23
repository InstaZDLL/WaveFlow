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
    """Yield `(line_number, block)` for each YAML list item.

    A crude split, and deliberately so: pulling in a YAML parser to read
    five workflow files would trade a dependency for a check that has to
    keep working on a bare runner. Steps are list items, so a new item
    at the same indentation or shallower ends the previous block.
    Non-step lists — a paths filter, a matrix — come out as blocks too
    and are simply never Rust steps.
    """
    block: list[str] = []
    start = 0
    indent = None

    for number, line in enumerate(text.splitlines(), start=1):
        match = LIST_ITEM_RE.match(line)
        if match and (indent is None or len(match.group(1)) <= indent):
            if block:
                yield start, "\n".join(block)
            block, start, indent = [line], number, len(match.group(1))
        elif block:
            block.append(line)

    if block:
        yield start, "\n".join(block)


def main() -> int:
    channel = pinned_channel()
    problems: list[str] = []
    checked = 0

    workflows = sorted(WORKFLOW_DIR.glob("*.yml")) + sorted(WORKFLOW_DIR.glob("*.yaml"))
    for workflow in workflows:
        rel = workflow.relative_to(ROOT).as_posix()
        text = workflow.read_text(encoding="utf-8")

        for number, line in enumerate(text.splitlines(), start=1):
            match = TOOLCHAIN_INPUT_RE.match(line)
            if not match or is_exempt(line):
                continue
            checked += 1
            value = match.group(1).strip("\"'")
            if value != channel:
                problems.append(
                    f"{rel}:{number}: installs {value}, but rust-toolchain.toml pins {channel}"
                )

        for number, block in steps(text):
            if not INSTALLS_RUST_RE.search(block):
                continue
            if any(TOOLCHAIN_INPUT_RE.match(line) for line in block.splitlines()):
                continue
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
    sys.exit(main())
