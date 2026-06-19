"""Fix the `player.controls.repeat{Off,All,One}` aria labels across all
17 locales.

Bug: the labels were describing the NEXT click's action instead of the
button's current state, against the standard ARIA toggle-button
pattern. This produced absurd readings — when the button is in
"Repeat All" mode the aria-label said "Repeat one" because that was
the next cycle target. Screen-reader users got the wrong state, and
the translation validator AI flagged this as a logical bug across
every translated locale.

Fix: relabel each key to describe the CURRENT state.
    repeatOff → button currently in OFF  → "Repeat off"
    repeatAll → button currently in ALL  → "Repeat all"
    repeatOne → button currently in ONE  → "Repeat one"

Idempotent — re-running on already-fixed locales is a no-op as long
as the canonical strings here match what's in the JSON.

Run from repo root:
    python scripts/fix_repeat_aria_labels.py
"""

import json
import sys
from pathlib import Path


REPLACEMENTS = {
    "fr": {
        "repeatOff": "Répétition désactivée",
        "repeatAll": "Répéter tout",
        "repeatOne": "Répéter une piste",
    },
    "en": {
        "repeatOff": "Repeat off",
        "repeatAll": "Repeat all",
        "repeatOne": "Repeat one",
    },
    "es": {
        "repeatOff": "Repetición desactivada",
        "repeatAll": "Repetir todo",
        "repeatOne": "Repetir una pista",
    },
    "de": {
        "repeatOff": "Wiederholung aus",
        "repeatAll": "Alle wiederholen",
        "repeatOne": "Einen Titel wiederholen",
    },
    "it": {
        "repeatOff": "Ripetizione disattivata",
        "repeatAll": "Ripeti tutto",
        "repeatOne": "Ripeti brano",
    },
    "nl": {
        "repeatOff": "Herhalen uit",
        "repeatAll": "Alles herhalen",
        "repeatOne": "Eén nummer herhalen",
    },
    "pt": {
        "repeatOff": "Repetição desativada",
        "repeatAll": "Repetir tudo",
        "repeatOne": "Repetir uma faixa",
    },
    "pt-BR": {
        "repeatOff": "Repetição desativada",
        "repeatAll": "Repetir tudo",
        "repeatOne": "Repetir uma faixa",
    },
    "ru": {
        "repeatOff": "Повтор выключен",
        "repeatAll": "Повторять все",
        "repeatOne": "Повторять один трек",
    },
    "tr": {
        "repeatOff": "Tekrar kapalı",
        "repeatAll": "Tümünü tekrarla",
        "repeatOne": "Bir parçayı tekrarla",
    },
    "id": {
        "repeatOff": "Ulang dimatikan",
        "repeatAll": "Ulang semua",
        "repeatOne": "Ulang satu lagu",
    },
    "ja": {
        "repeatOff": "リピート オフ",
        "repeatAll": "すべてリピート",
        "repeatOne": "1曲リピート",
    },
    "ko": {
        "repeatOff": "반복 끔",
        "repeatAll": "전체 반복",
        "repeatOne": "한 곡 반복",
    },
    "zh-CN": {
        "repeatOff": "重复关闭",
        "repeatAll": "全部重复",
        "repeatOne": "单曲重复",
    },
    "zh-TW": {
        "repeatOff": "重複關閉",
        "repeatAll": "全部重複",
        "repeatOne": "單曲重複",
    },
    "ar": {
        "repeatOff": "التكرار معطّل",
        "repeatAll": "تكرار الكل",
        "repeatOne": "تكرار مقطع واحد",
    },
    "hi": {
        "repeatOff": "दोहराव बंद",
        "repeatAll": "सभी दोहराएँ",
        "repeatOne": "एक ट्रैक दोहराएँ",
    },
}


def patch_locale(path: Path, new_values: dict) -> bool:
    with path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)
    controls = data.setdefault("player", {}).setdefault("controls", {})
    changed = False
    for key, value in new_values.items():
        if controls.get(key) != value:
            controls[key] = value
            changed = True
    if not changed:
        return False
    with path.open("w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    return True


def main() -> int:
    here = Path(__file__).resolve().parent.parent / "src" / "i18n" / "locales"
    changed: list[str] = []
    skipped: list[str] = []
    for code, values in REPLACEMENTS.items():
        path = here / f"{code}.json"
        if not path.exists():
            skipped.append(f"{code}(missing)")
            continue
        if patch_locale(path, values):
            changed.append(code)
        else:
            skipped.append(code)
    sys.stdout.reconfigure(encoding="utf-8")
    print(f"changed ({len(changed)}): {', '.join(changed) or '(none)'}")
    print(f"skipped ({len(skipped)}): {', '.join(skipped) or '(none)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
