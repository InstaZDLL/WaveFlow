"""One-shot script: propagate `onboarding.appearance` keys to all 17 locales.

Run from repo root:
    python scripts/add_appearance_step_i18n.py

The script is idempotent — re-running on a locale that already has the
block is a no-op. Brand tokens (WaveFlow, Last.fm, Deezer, ReplayGain,
LRCLIB, BPM) stay verbatim per project convention.
"""

import json
import sys
from pathlib import Path

# Translation matrix. fr is the source of truth from
# `src/i18n/locales/fr.json`.
TRANSLATIONS = {
    "fr": {
        "title": "Choisissez votre look",
        "description": "Sélectionnez un thème et un style d'interface. Vous pouvez les changer à tout moment dans Réglages → Apparence.",
        "theme_title": "Thème de couleurs",
        "skin_title": "Style d'interface",
        "hint": "Ces choix sont enregistrés sur ce profil — chaque profil peut avoir son propre look.",
    },
    "en": {
        "title": "Choose your look",
        "description": "Pick a colour theme and an interface style. You can change them anytime from Settings → Appearance.",
        "theme_title": "Colour theme",
        "skin_title": "Interface style",
        "hint": "These choices are saved per profile — every profile can have its own look.",
    },
    "es": {
        "title": "Elige tu estilo",
        "description": "Selecciona un tema de color y un estilo de interfaz. Puedes cambiarlos en cualquier momento desde Ajustes → Apariencia.",
        "theme_title": "Tema de color",
        "skin_title": "Estilo de interfaz",
        "hint": "Estas opciones se guardan por perfil — cada perfil puede tener su propio estilo.",
    },
    "de": {
        "title": "Wählen Sie Ihren Look",
        "description": "Wählen Sie ein Farbschema und einen Oberflächenstil. Sie können beides jederzeit unter Einstellungen → Erscheinungsbild ändern.",
        "theme_title": "Farbschema",
        "skin_title": "Oberflächenstil",
        "hint": "Diese Auswahl wird pro Profil gespeichert — jedes Profil kann sein eigenes Aussehen haben.",
    },
    "it": {
        "title": "Scegli il tuo stile",
        "description": "Seleziona un tema di colore e uno stile d'interfaccia. Puoi cambiarli in qualsiasi momento da Impostazioni → Aspetto.",
        "theme_title": "Tema dei colori",
        "skin_title": "Stile d'interfaccia",
        "hint": "Queste scelte sono salvate per profilo — ogni profilo può avere il suo aspetto.",
    },
    "nl": {
        "title": "Kies je look",
        "description": "Selecteer een kleurthema en een interfacestijl. Je kunt ze altijd wijzigen via Instellingen → Uiterlijk.",
        "theme_title": "Kleurthema",
        "skin_title": "Interfacestijl",
        "hint": "Deze keuzes worden per profiel opgeslagen — elk profiel kan zijn eigen look hebben.",
    },
    "pt": {
        "title": "Escolha o seu look",
        "description": "Selecione um tema de cor e um estilo de interface. Pode alterá-los a qualquer momento em Definições → Aparência.",
        "theme_title": "Tema de cor",
        "skin_title": "Estilo de interface",
        "hint": "Estas escolhas são guardadas por perfil — cada perfil pode ter o seu próprio look.",
    },
    "pt-BR": {
        "title": "Escolha seu visual",
        "description": "Selecione um tema de cor e um estilo de interface. Você pode alterá-los a qualquer momento em Configurações → Aparência.",
        "theme_title": "Tema de cor",
        "skin_title": "Estilo de interface",
        "hint": "Essas escolhas são salvas por perfil — cada perfil pode ter seu próprio visual.",
    },
    "ru": {
        "title": "Выберите свой стиль",
        "description": "Выберите цветовую тему и стиль интерфейса. Их можно изменить в любой момент в разделе Настройки → Внешний вид.",
        "theme_title": "Цветовая тема",
        "skin_title": "Стиль интерфейса",
        "hint": "Эти настройки сохраняются для каждого профиля — у каждого профиля может быть свой стиль.",
    },
    "tr": {
        "title": "Görünümünüzü seçin",
        "description": "Bir renk teması ve arayüz stili seçin. Bunları istediğiniz zaman Ayarlar → Görünüm bölümünden değiştirebilirsiniz.",
        "theme_title": "Renk teması",
        "skin_title": "Arayüz stili",
        "hint": "Bu seçimler her profil için kaydedilir — her profilin kendi görünümü olabilir.",
    },
    "id": {
        "title": "Pilih tampilan Anda",
        "description": "Pilih tema warna dan gaya antarmuka. Anda dapat mengubahnya kapan saja melalui Pengaturan → Tampilan.",
        "theme_title": "Tema warna",
        "skin_title": "Gaya antarmuka",
        "hint": "Pilihan ini disimpan per profil — setiap profil dapat memiliki tampilannya sendiri.",
    },
    "ja": {
        "title": "外観を選ぶ",
        "description": "カラーテーマとインターフェーススタイルを選んでください。設定 → 外観からいつでも変更できます。",
        "theme_title": "カラーテーマ",
        "skin_title": "インターフェーススタイル",
        "hint": "この選択はプロファイルごとに保存されます — 各プロファイルは独自の外観を持つことができます。",
    },
    "ko": {
        "title": "외관 선택",
        "description": "컬러 테마와 인터페이스 스타일을 선택하세요. 설정 → 외관에서 언제든지 변경할 수 있습니다.",
        "theme_title": "컬러 테마",
        "skin_title": "인터페이스 스타일",
        "hint": "이 선택은 프로필별로 저장됩니다 — 각 프로필은 고유한 외관을 가질 수 있습니다.",
    },
    "zh-CN": {
        "title": "选择您的外观",
        "description": "选择一个颜色主题和界面风格。您可以随时在 设置 → 外观 中更改。",
        "theme_title": "颜色主题",
        "skin_title": "界面风格",
        "hint": "这些选项按配置文件保存 — 每个配置文件都可以有自己的外观。",
    },
    "zh-TW": {
        "title": "選擇您的外觀",
        "description": "選擇一個顏色主題和介面風格。您可以隨時在 設定 → 外觀 中變更。",
        "theme_title": "顏色主題",
        "skin_title": "介面風格",
        "hint": "這些選項依設定檔儲存 — 每個設定檔都可以擁有自己的外觀。",
    },
    "ar": {
        "title": "اختر مظهرك",
        "description": "اختر سمة لونية ونمط واجهة. يمكنك تغييرهما في أي وقت من الإعدادات → المظهر.",
        "theme_title": "السمة اللونية",
        "skin_title": "نمط الواجهة",
        "hint": "تُحفظ هذه الخيارات لكل ملف تعريف — يمكن أن يكون لكل ملف تعريف مظهره الخاص.",
    },
    "hi": {
        "title": "अपना लुक चुनें",
        "description": "एक रंग थीम और इंटरफ़ेस शैली चुनें। आप इन्हें कभी भी सेटिंग्स → उपस्थिति से बदल सकते हैं।",
        "theme_title": "रंग थीम",
        "skin_title": "इंटरफ़ेस शैली",
        "hint": "ये विकल्प प्रति प्रोफ़ाइल सहेजे जाते हैं — प्रत्येक प्रोफ़ाइल का अपना लुक हो सकता है।",
    },
}


def patch_locale(path: Path, translations: dict) -> bool:
    """Insert the appearance block after `onboarding.localOnly`.

    Returns True when the file was modified, False when already up to date.
    """
    with path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)

    onboarding = data.setdefault("onboarding", {})
    if "appearance" in onboarding:
        return False  # idempotent — leave hand-edited value alone

    block = {
        "title": translations["title"],
        "description": translations["description"],
        "theme": {"title": translations["theme_title"]},
        "skin": {"title": translations["skin_title"]},
        "hint": translations["hint"],
    }

    # Rebuild `onboarding` in stable key order so `appearance` lands
    # right after `localOnly`. json.dump preserves dict insertion order
    # in Python 3.7+, and reordering keeps the diff readable.
    order = []
    inserted = False
    for key in onboarding:
        order.append(key)
        if key == "localOnly" and not inserted:
            order.append("appearance")
            inserted = True
    if not inserted:
        # `localOnly` wasn't present (shouldn't happen) — append at end.
        order.append("appearance")

    onboarding["appearance"] = block
    data["onboarding"] = {k: onboarding[k] for k in order}

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
