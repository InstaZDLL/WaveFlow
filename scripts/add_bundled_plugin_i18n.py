"""One-shot script: propagate i18n strings introduced by the
"bundled plugin uninstall guard" PR (Web Radio polish).

Adds:
  - `settings.plugins.bundled` / `bundledHint`
  - `webRadio.unavailableTitle` / `unavailableHint`

across all 17 locales. Idempotent — re-running on a locale that
already has the keys is a no-op.

Run from repo root:
    python scripts/add_bundled_plugin_i18n.py
"""

import json
import sys
from pathlib import Path

PLUGINS_BUNDLED_KEY = "bundled"
PLUGINS_BUNDLED_HINT_KEY = "bundledHint"
WEBRADIO_UNAVAILABLE_TITLE_KEY = "unavailableTitle"
WEBRADIO_UNAVAILABLE_HINT_KEY = "unavailableHint"

TRANSLATIONS = {
    "fr": {
        "bundled": "Inclus",
        "bundledHint": "Livré avec WaveFlow — désactivez-le pour le masquer.",
        "unavailableTitle": "Web Radio désactivée",
        "unavailableHint": "Activez le plugin Web Radio dans Réglages → Plugins pour parcourir plus de 30 000 stations.",
    },
    "en": {
        "bundled": "Built-in",
        "bundledHint": "Ships with WaveFlow — disable it to hide it.",
        "unavailableTitle": "Web Radio is disabled",
        "unavailableHint": "Enable the Web Radio plugin in Settings → Plugins to browse 30,000+ stations.",
    },
    "es": {
        "bundled": "Integrado",
        "bundledHint": "Incluido con WaveFlow — desactívalo para ocultarlo.",
        "unavailableTitle": "Web Radio está desactivada",
        "unavailableHint": "Activa el plugin de Web Radio en Ajustes → Plugins para explorar más de 30 000 estaciones.",
    },
    "de": {
        "bundled": "Mitgeliefert",
        "bundledHint": "Mit WaveFlow ausgeliefert — deaktivieren, um es auszublenden.",
        "unavailableTitle": "Web Radio ist deaktiviert",
        "unavailableHint": "Aktivieren Sie das Web Radio-Plugin in Einstellungen → Plugins, um über 30 000 Sender zu durchsuchen.",
    },
    "it": {
        "bundled": "Integrato",
        "bundledHint": "Incluso in WaveFlow — disattivalo per nasconderlo.",
        "unavailableTitle": "Web Radio è disattivata",
        "unavailableHint": "Attiva il plugin Web Radio in Impostazioni → Plugin per sfogliare oltre 30 000 stazioni.",
    },
    "nl": {
        "bundled": "Ingebouwd",
        "bundledHint": "Meegeleverd met WaveFlow — schakel het uit om het te verbergen.",
        "unavailableTitle": "Web Radio is uitgeschakeld",
        "unavailableHint": "Schakel de Web Radio-plugin in via Instellingen → Plugins om meer dan 30.000 stations te doorzoeken.",
    },
    "pt": {
        "bundled": "Integrado",
        "bundledHint": "Incluído no WaveFlow — desative-o para o ocultar.",
        "unavailableTitle": "Web Radio está desativada",
        "unavailableHint": "Ative o plugin Web Radio em Definições → Plugins para explorar mais de 30 000 estações.",
    },
    "pt-BR": {
        "bundled": "Integrado",
        "bundledHint": "Incluído com o WaveFlow — desative para ocultá-lo.",
        "unavailableTitle": "Web Radio está desativada",
        "unavailableHint": "Ative o plugin Web Radio em Configurações → Plugins para explorar mais de 30 000 estações.",
    },
    "ru": {
        "bundled": "Встроенный",
        "bundledHint": "Поставляется с WaveFlow — отключите, чтобы скрыть.",
        "unavailableTitle": "Web Radio отключено",
        "unavailableHint": "Включите плагин Web Radio в Настройки → Плагины, чтобы просматривать более 30 000 станций.",
    },
    "tr": {
        "bundled": "Yerleşik",
        "bundledHint": "WaveFlow ile birlikte gelir — gizlemek için devre dışı bırakın.",
        "unavailableTitle": "Web Radio devre dışı",
        "unavailableHint": "30.000'den fazla istasyona göz atmak için Ayarlar → Plugin'ler bölümünden Web Radio plugin'ini etkinleştirin.",
    },
    "id": {
        "bundled": "Bawaan",
        "bundledHint": "Disertakan dengan WaveFlow — nonaktifkan untuk menyembunyikannya.",
        "unavailableTitle": "Web Radio dinonaktifkan",
        "unavailableHint": "Aktifkan plugin Web Radio di Pengaturan → Plugin untuk menjelajahi lebih dari 30.000 stasiun.",
    },
    "ja": {
        "bundled": "組み込み",
        "bundledHint": "WaveFlow に同梱されています — 非表示にするには無効にしてください。",
        "unavailableTitle": "Web Radio は無効です",
        "unavailableHint": "30,000 以上の放送局を閲覧するには、設定 → プラグイン で Web Radio プラグインを有効にしてください。",
    },
    "ko": {
        "bundled": "내장",
        "bundledHint": "WaveFlow에 포함되어 있습니다 — 숨기려면 비활성화하세요.",
        "unavailableTitle": "Web Radio가 비활성화됨",
        "unavailableHint": "30,000개 이상의 방송국을 탐색하려면 설정 → 플러그인에서 Web Radio 플러그인을 활성화하세요.",
    },
    "zh-CN": {
        "bundled": "内置",
        "bundledHint": "随 WaveFlow 提供 — 禁用以隐藏。",
        "unavailableTitle": "Web Radio 已禁用",
        "unavailableHint": "在 设置 → 插件 中启用 Web Radio 插件以浏览超过 30,000 个电台。",
    },
    "zh-TW": {
        "bundled": "內建",
        "bundledHint": "隨 WaveFlow 提供 — 停用以隱藏。",
        "unavailableTitle": "Web Radio 已停用",
        "unavailableHint": "在 設定 → 外掛 中啟用 Web Radio 外掛以瀏覽超過 30,000 個電台。",
    },
    "ar": {
        "bundled": "مدمج",
        "bundledHint": "يأتي مع WaveFlow — قم بتعطيله لإخفائه.",
        "unavailableTitle": "Web Radio معطّل",
        "unavailableHint": "فعّل إضافة Web Radio من الإعدادات → الإضافات لتصفح أكثر من 30,000 محطة.",
    },
    "hi": {
        "bundled": "अंतर्निहित",
        "bundledHint": "WaveFlow के साथ शामिल — छुपाने के लिए अक्षम करें।",
        "unavailableTitle": "Web Radio अक्षम है",
        "unavailableHint": "30,000+ स्टेशन ब्राउज़ करने के लिए सेटिंग्स → प्लगइन्स में Web Radio प्लगइन सक्षम करें।",
    },
}


def patch_locale(path: Path, translations: dict) -> bool:
    """Insert the bundled + webRadio.unavailable* keys. Returns True
    when the file was modified."""
    with path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)

    changed = False

    settings = data.setdefault("settings", {})
    plugins = settings.setdefault("plugins", {})
    if PLUGINS_BUNDLED_KEY not in plugins:
        plugins[PLUGINS_BUNDLED_KEY] = translations["bundled"]
        changed = True
    if PLUGINS_BUNDLED_HINT_KEY not in plugins:
        plugins[PLUGINS_BUNDLED_HINT_KEY] = translations["bundledHint"]
        changed = True

    webradio = data.setdefault("webRadio", {})
    if WEBRADIO_UNAVAILABLE_TITLE_KEY not in webradio:
        webradio[WEBRADIO_UNAVAILABLE_TITLE_KEY] = translations["unavailableTitle"]
        changed = True
    if WEBRADIO_UNAVAILABLE_HINT_KEY not in webradio:
        webradio[WEBRADIO_UNAVAILABLE_HINT_KEY] = translations["unavailableHint"]
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
    for code, translations in TRANSLATIONS.items():
        path = here / f"{code}.json"
        if not path.exists():
            print(f"  skip {code} — file missing")
            continue
        if patch_locale(path, translations):
            changed.append(code)
        else:
            skipped.append(code)
    print(f"changed ({len(changed)}): {', '.join(changed) or '(none)'}")
    print(f"skipped ({len(skipped)}): {', '.join(skipped) or '(none)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
