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
- every workflow that installs Rust actually passes one, so a step
  cannot drift back to an implicit `stable`.

A line ending in `# toolchain-pin: exempt` is skipped, for the day a
workflow genuinely needs a different compiler.

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
EXEMPT = "# toolchain-pin: exempt"


def pinned_channel() -> str:
    if not TOOLCHAIN_FILE.exists():
        sys.exit(f"missing {TOOLCHAIN_FILE.relative_to(ROOT)}")
    match = CHANNEL_RE.search(TOOLCHAIN_FILE.read_text(encoding="utf-8"))
    if not match:
        sys.exit(f"no `channel = \"...\"` in {TOOLCHAIN_FILE.relative_to(ROOT)}")
    return match.group(1)


def main() -> int:
    channel = pinned_channel()
    problems: list[str] = []
    checked = 0

    for workflow in sorted(WORKFLOW_DIR.glob("*.yml")) + sorted(WORKFLOW_DIR.glob("*.yaml")):
        rel = workflow.relative_to(ROOT).as_posix()
        text = workflow.read_text(encoding="utf-8")
        installs_rust = bool(INSTALLS_RUST_RE.search(text))
        found_here = 0

        for number, line in enumerate(text.splitlines(), start=1):
            match = TOOLCHAIN_INPUT_RE.match(line)
            if not match:
                continue
            # Counted before the exemption is honoured: an exempted line
            # is still a `toolchain:` input, so it must satisfy the
            # "installs Rust without pinning" rule below. Skipping it
            # outright made the escape hatch trip the other check.
            found_here += 1
            if EXEMPT in line:
                continue
            checked += 1
            value = match.group(1).strip("\"'")
            if value != channel:
                problems.append(
                    f"{rel}:{number}: installs {value}, but rust-toolchain.toml pins {channel}"
                )

        if installs_rust and found_here == 0:
            problems.append(
                f"{rel}: installs Rust without a `toolchain:` input, so it resolves "
                f"to whatever the action defaults to instead of {channel}"
            )

    if problems:
        print("Toolchain pin is not consistent:\n", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        print(
            f"\nBump every site together, or mark a deliberate exception with "
            f"`{EXEMPT}` on the line.",
            file=sys.stderr,
        )
        return 1

    print(f"toolchain pin: {checked} workflow input(s) all on {channel}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
