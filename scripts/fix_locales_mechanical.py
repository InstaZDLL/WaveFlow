"""Mechanical i18n cleanup — cluster 3 (post-validator pass-2).

Two systemic bugs the AI validator surfaced after #268 + #269 landed:

1. **fr `_zero` keys at singular instead of plural.** French grammar
   requires a plural noun after `0` (unlike English, where `0 track`
   is singular). The first JSON authoring pass borrowed the English
   plural rules and parked every `_zero` form at singular, which the
   validator caught across ~25 keys.

2. **zh-CN + zh-TW half-width ASCII punctuation inside CJK text.**
   CJK conventions use full-width `，` `：` `（` `）` between Chinese
   characters; the source JSON drifted to half-width `,` `:` `(` `)`
   in many places. The validator flagged this in both Chinese
   locales.

Idempotent — re-running on already-clean files is a no-op. Returns
non-zero only when a locale file the script expected isn't on disk.

Run from repo root:
    python scripts/fix_locales_mechanical.py
"""

import json
import re
import sys
from pathlib import Path


# --- Section 1 — French `_zero` pluralisation ------------------------

# Map of singular → plural noun stems. The script walks every leaf
# string whose key ends with `_zero` and, when the value starts with
# the singular stem, rewrites it to the plural. Stems that the
# validator did NOT flag (idiomatic alternatives like `"Aucun résultat"`
# or `"Rien à supprimer"`) are deliberately absent from this map so
# the script leaves them alone.
FR_ZERO_FIXES = {
    "0 titre": "0 titres",
    "0 album": "0 albums",
    "0 artiste": "0 artistes",
    "0 genre": "0 genres",
    "0 dossier": "0 dossiers",
    "0 écoute": "0 écoutes",
    "0 minute de musique": "0 minutes de musique",
    "0 jour d'affilée": "0 jours d'affilée",
    "0 sélectionné": "0 sélectionnés",
}


def walk(obj, path=""):
    """Recursively yield (dotted-path, value) for every leaf string."""
    if isinstance(obj, dict):
        for k, v in obj.items():
            yield from walk(v, f"{path}.{k}" if path else k)
    elif isinstance(obj, str):
        yield path, obj


def set_path(data, dotted, value):
    cursor = data
    parts = dotted.split(".")
    for part in parts[:-1]:
        cursor = cursor[part]
    cursor[parts[-1]] = value


def fix_fr_plurals(data) -> int:
    """Walk the loaded fr.json tree and apply the singular→plural map
    to every `_zero` leaf. Returns the number of keys mutated."""
    fixed = 0
    for path, value in list(walk(data)):
        if not path.endswith("_zero"):
            continue
        new_value = FR_ZERO_FIXES.get(value)
        if new_value is None or new_value == value:
            continue
        set_path(data, path, new_value)
        fixed += 1
    return fixed


# --- Section 2 — CJK punctuation -------------------------------------

# `一-鿿` is the CJK Unified Ideographs block — enough for
# both zh-CN and zh-TW common ideographs. Half-width punctuation that
# appears in a CJK context should be replaced with the full-width
# variant.
CJK = r"[一-鿿]"

# Patterns we touch unconditionally when both sides are CJK. The
# question / bang variants also fire when the punctuation lands at
# end-of-string after a CJK character — those still belong in
# full-width form even when not followed by more CJK content.
_COMMA_RE = re.compile(rf"(?<={CJK}),(?={CJK})")
_COLON_RE = re.compile(rf"(?<={CJK}):(?={CJK})")
_QMARK_RE = re.compile(rf"(?<={CJK})\?(?={CJK}|$)")
_BANG_RE = re.compile(rf"(?<={CJK})!(?={CJK}|$)")

# Parentheses run TWO directions to enforce the same convention:
#
# 1. Half-width → full-width when the paren follows CJK AND the
#    content inside is purely CJK. Keeps `(A = {{a}})` and
#    `(A → Z)` style annotations at half-width — that's the CJK
#    convention for Latin/expression parentheticals.
# 2. Full-width → half-width when the paren content carries any
#    Latin / digit / template-variable character. Cleans up
#    pre-existing strings like `（MP3、FLAC、ALAC、…）` or
#    `（{{count}} 个字段）` where the original authoring pass used
#    full-width parens around mixed content. Same rule as above,
#    just applied in the opposite direction.
_PAREN_HW_RE = re.compile(rf"({CJK})\(([^()]*)\)")
_PAREN_FW_RE = re.compile(r"（([^（）]*)）")

# Anything that disqualifies content from full-width parens.
_PAREN_NON_CJK = re.compile(r"[A-Za-z0-9{}/=→]")


def _paren_hw_to_fw(match: re.Match) -> str:
    before = match.group(1)
    content = match.group(2)
    has_cjk = re.search(CJK, content) is not None
    has_non_cjk = _PAREN_NON_CJK.search(content) is not None
    if has_cjk and not has_non_cjk:
        return f"{before}（{content}）"
    return match.group(0)


def _paren_fw_to_hw(match: re.Match) -> str:
    content = match.group(1)
    if _PAREN_NON_CJK.search(content):
        return f"({content})"
    return match.group(0)


def fix_cjk_punctuation(data) -> int:
    """Walk a CJK locale tree and replace half-width punctuation
    occurring in CJK contexts with the full-width variant. Returns
    the number of leaf strings touched."""
    touched = 0
    for path, value in list(walk(data)):
        new_value = _COMMA_RE.sub("，", value)
        new_value = _COLON_RE.sub("：", new_value)
        new_value = _QMARK_RE.sub("？", new_value)
        new_value = _BANG_RE.sub("！", new_value)
        new_value = _PAREN_HW_RE.sub(_paren_hw_to_fw, new_value)
        new_value = _PAREN_FW_RE.sub(_paren_fw_to_hw, new_value)
        if new_value != value:
            set_path(data, path, new_value)
            touched += 1
    return touched


# --- Wire it ---------------------------------------------------------

def patch_locale(path: Path, code: str) -> int:
    """Apply the appropriate fixer(s) for `code` to the file at
    `path`. Returns the number of leaf strings mutated, -1 if `code`
    has no scheduled fixes, or -2 when the file failed to parse as
    JSON (caller treats this as fatal alongside `missing`)."""
    try:
        with path.open("r", encoding="utf-8") as fh:
            data = json.load(fh)
    except json.JSONDecodeError as err:
        print(
            f"  parse error in {path}: line {err.lineno} col {err.colno}: {err.msg}",
            file=sys.stderr,
        )
        return -2
    if code == "fr":
        touched = fix_fr_plurals(data)
    elif code in ("zh-CN", "zh-TW"):
        touched = fix_cjk_punctuation(data)
    else:
        return -1
    if touched == 0:
        return 0
    with path.open("w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return touched


def main() -> int:
    here = Path(__file__).resolve().parent.parent / "src" / "i18n" / "locales"
    sys.stdout.reconfigure(encoding="utf-8")
    changed: list[tuple[str, int]] = []
    skipped: list[str] = []
    missing: list[str] = []
    unparseable: list[str] = []
    for code in ("fr", "zh-CN", "zh-TW"):
        path = here / f"{code}.json"
        if not path.exists():
            missing.append(code)
            continue
        touched = patch_locale(path, code)
        if touched == -2:
            unparseable.append(code)
        elif touched > 0:
            changed.append((code, touched))
        else:
            skipped.append(code)
    changed_summary = (
        ", ".join(f"{code} ({n})" for code, n in changed) if changed else "(none)"
    )
    print(f"changed ({len(changed)}): {changed_summary}")
    print(f"skipped ({len(skipped)}): {', '.join(skipped) or '(none)'}")
    if missing:
        print(
            f"missing ({len(missing)}): {', '.join(missing)} — "
            "expected locale file(s) not found",
            file=sys.stderr,
        )
    if unparseable:
        print(
            f"unparseable ({len(unparseable)}): {', '.join(unparseable)} — "
            "JSON parse failed (see lines above)",
            file=sys.stderr,
        )
    return 1 if (missing or unparseable) else 0


if __name__ == "__main__":
    sys.exit(main())
