"""Apply the per-locale "Critical fixes" tables emitted by the
translation validator to every locale JSON file.

Walks the latest report directory under
`secrets/translation-validation-reports/<timestamp>/<code>.md`,
parses each report's "Critical fixes" markdown table, and writes the
Suggested target column back into `src/i18n/locales/<code>.json` at
the dotted JSON path from the first column.

Rules / safety:
- Idempotent. Re-running on already-aligned files is a no-op.
- Patches that the AI proposes but whose CURRENT value already matches
  the suggested target are silently skipped (covered by an earlier
  cleanup PR).
- Patches whose target JSON path doesn't exist in the locale file
  are reported but not applied — surfaces stale paths the AI
  hallucinated against an older schema.
- Strings are unwrapped from their surrounding double quotes per the
  markdown table convention `"value"` → `value`.
- Pipe `|` characters inside a cell are rare but must be escaped
  to backslash-pipe. We honour that on read.

Run from repo root:
    python scripts/apply_validator_fixes.py
    python scripts/apply_validator_fixes.py --report 2026-06-19T15-48-36-005Z
    python scripts/apply_validator_fixes.py --dry-run
"""

import argparse
import json
import re
import sys
from pathlib import Path


REPORTS_ROOT_REL = Path("secrets") / "translation-validation-reports"
LOCALES_REL = Path("src") / "i18n" / "locales"


def latest_report_dir(root: Path) -> Path:
    """Return the most recent ISO-8601 timestamp folder, or raise."""
    dirs = [p for p in root.iterdir() if p.is_dir()]
    if not dirs:
        raise SystemExit(f"no report directories under {root}")
    dirs.sort(key=lambda p: p.name)
    return dirs[-1]


# Strings that CLAUDE.md formalises as untranslated brand-like tokens.
# Validator AIs frequently propose translating them; we skip every patch
# whose suggested target drops a brand token the current value still
# carries, so this PR can't accidentally re-litigate the convention.
BRAND_TOKENS = (
    "WaveFlow",
    "Last.fm",
    "Deezer",
    "ReplayGain",
    "LRCLIB",
    "BPM",
    "Daily Mix",
    "On Repeat",
)


def parse_critical_fixes(md: str) -> list[tuple[str, str]]:
    """Return the list of (json_path, suggested_target) rows in the
    `### 2. Critical fixes` section. Empty list when the section is
    missing or carries no table."""
    # Slice from "### 2." to the next "### " header (or end-of-file).
    m = re.search(r"^### 2\..*?$", md, flags=re.MULTILINE)
    if not m:
        return []
    start = m.end()
    next_header = re.search(r"^### \d", md[start:], flags=re.MULTILINE)
    block = md[start : start + next_header.start()] if next_header else md[start:]

    rows: list[tuple[str, str]] = []
    for line in block.splitlines():
        line = line.strip()
        if not line.startswith("|") or not line.endswith("|"):
            continue
        # Skip header row and separator row.
        if "---" in line:
            continue
        # Split on `|` while honouring escaped pipes (backslash-pipe).
        # Negative lookbehind: split on every `|` not preceded by a
        # backslash, then drop the backslash from any escaped pipe
        # that ended up inside a cell.
        cells = [
            cell.strip().replace("\\|", "|") for cell in re.split(r"(?<!\\)\|", line)
        ]
        # `|a|b|c|` → ['', 'a', 'b', 'c', '']
        cells = [c for c in cells if c != ""]
        if len(cells) < 5:
            continue
        # First row is the header; skip it.
        if cells[0].lower() == "json path":
            continue
        path = cells[0]
        target = cells[-1]
        # Strip surrounding double quotes from the suggested target.
        if target.startswith('"') and target.endswith('"') and len(target) >= 2:
            target = target[1:-1]
        # Markdown escapes inside the value:
        #   `\"` → `"`, `\\` → `\`, `\|` was already handled.
        target = target.replace(r"\"", '"').replace(r"\\", "\\")
        if not path:
            continue
        # The AI sometimes emits "(no change)" or "—" when no fix is
        # actually proposed for a row; drop those.
        if target in ("(no change)", "—", "-", "n/a", "N/A", ""):
            continue
        rows.append((path, target))
    return rows


def get_path(data: dict, dotted: str):
    cursor = data
    for part in dotted.split("."):
        if not isinstance(cursor, dict) or part not in cursor:
            return None
        cursor = cursor[part]
    return cursor


def set_path(data: dict, dotted: str, value: str) -> bool:
    """Write `value` at `dotted`. Returns False when the path is not
    fully present (avoid creating new keys silently)."""
    cursor = data
    parts = dotted.split(".")
    for part in parts[:-1]:
        if not isinstance(cursor, dict) or part not in cursor:
            return False
        cursor = cursor[part]
    leaf = parts[-1]
    if not isinstance(cursor, dict) or leaf not in cursor:
        return False
    cursor[leaf] = value
    return True


def apply_locale(report_path: Path, locale_path: Path, dry_run: bool):
    """Returns (applied, unchanged, missing) counts for this locale."""
    md = report_path.read_text(encoding="utf-8")
    rows = parse_critical_fixes(md)
    if not rows:
        return 0, 0, 0
    try:
        with locale_path.open("r", encoding="utf-8") as fh:
            data = json.load(fh)
    except json.JSONDecodeError as err:
        print(
            f"  parse error in {locale_path}: line {err.lineno} col {err.colno}: {err.msg}",
            file=sys.stderr,
        )
        return -1, -1, -1

    applied = 0
    unchanged = 0
    missing = 0
    missing_paths: list[str] = []
    skipped_brand: list[str] = []
    for path, target in rows:
        current = get_path(data, path)
        if current is None:
            missing += 1
            missing_paths.append(path)
            continue
        if current == target:
            unchanged += 1
            continue
        # Brand-token guard: don't let the validator drop tokens
        # CLAUDE.md keeps verbatim across locales. Skip the patch
        # entirely so the current value (which still carries the
        # token) stays put.
        dropped = [
            token
            for token in BRAND_TOKENS
            if token in current and token not in target
        ]
        if dropped:
            skipped_brand.append(f"{path} [{', '.join(dropped)}]")
            continue
        if not dry_run:
            ok = set_path(data, path, target)
            if not ok:
                missing += 1
                missing_paths.append(path)
                continue
        applied += 1

    if skipped_brand:
        print(
            f"  {locale_path.stem}: {len(skipped_brand)} patch(es) "
            "skipped (would drop a brand token)",
            file=sys.stderr,
        )
        for entry in skipped_brand[:5]:
            print(f"    - {entry}", file=sys.stderr)
        if len(skipped_brand) > 5:
            print(f"    … and {len(skipped_brand) - 5} more", file=sys.stderr)

    if applied > 0 and not dry_run:
        with locale_path.open("w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=2)
            fh.write("\n")

    if missing_paths:
        print(
            f"  {locale_path.stem}: {len(missing_paths)} stale path(s) ignored",
            file=sys.stderr,
        )
        for p in missing_paths[:5]:
            print(f"    - {p}", file=sys.stderr)
        if len(missing_paths) > 5:
            print(f"    … and {len(missing_paths) - 5} more", file=sys.stderr)

    return applied, unchanged, missing


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--report",
        help="Report timestamp folder under secrets/translation-validation-reports/. "
        "Defaults to the most recent one.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Walk the reports and count proposed changes without writing.",
    )
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    reports_root = root / REPORTS_ROOT_REL
    if args.report:
        report_dir = reports_root / args.report
    else:
        report_dir = latest_report_dir(reports_root)
    locales_root = root / LOCALES_REL

    print(f"reading reports from {report_dir.relative_to(root)}")
    if args.dry_run:
        print("(dry-run — no files will be written)")

    total_applied = 0
    total_unchanged = 0
    total_missing = 0
    per_locale: list[tuple[str, int, int, int]] = []
    failures: list[str] = []
    for md_path in sorted(report_dir.glob("*.md")):
        code = md_path.stem
        locale_path = locales_root / f"{code}.json"
        if not locale_path.exists():
            failures.append(f"{code}(no locale file)")
            continue
        applied, unchanged, missing = apply_locale(md_path, locale_path, args.dry_run)
        if applied < 0:
            failures.append(f"{code}(parse error)")
            continue
        per_locale.append((code, applied, unchanged, missing))
        total_applied += applied
        total_unchanged += unchanged
        total_missing += missing

    print()
    print(f"{'locale':<8} {'applied':>8} {'unchanged':>10} {'stale-path':>11}")
    for code, applied, unchanged, missing in per_locale:
        print(f"{code:<8} {applied:>8} {unchanged:>10} {missing:>11}")
    print(
        f"{'TOTAL':<8} {total_applied:>8} {total_unchanged:>10} {total_missing:>11}"
    )

    if failures:
        print()
        print(f"failures: {', '.join(failures)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
