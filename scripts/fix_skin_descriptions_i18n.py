"""Fix the skin-description franglais / MT artefacts across the
locales the translation validator flagged.

Targets `settings.appearance.skin.subtitle` and
`settings.appearance.skins.<id>.description` only — the skin labels
themselves (`Studio`, `Editorial`, `Lounge`, `Pulse`, `Liquid`) stay
verbatim across every locale because they're proper-noun feature
names (same convention as `Daily Mix` / `On Repeat`).

Values copied verbatim from the per-locale "Suggested target" column
in the AI translation reports under
`secrets/translation-validation-reports/`. Idempotent — re-running
on a locale already aligned is a no-op.

Run from repo root:
    python scripts/fix_skin_descriptions_i18n.py
"""

import json
import sys
from pathlib import Path


# Per-locale dotted-path → new value. Paths are flat ascii so we can
# split on `.` cleanly.
PATCHES = {
    "fr": {
        "settings.appearance.skin.subtitle": "Modifie la densité, les matériaux, la typographie et les animations — pas seulement les couleurs.",
        "settings.appearance.skins.studio.description": "Ambiance Apple Music — interface dense, ombres douces, sans empattement épuré.",
        "settings.appearance.skins.editorial.description": "Magazine sur papier — serif éditorial, fines lignes de séparation, grain doux, espace généreux.",
        "settings.appearance.skins.lounge.description": "Glass premium — la pochette de l'album devient l'arrière-plan, panneaux translucides, transitions douces.",
        "settings.appearance.skins.pulse.description": "Club néon — base OLED, halos colorés autour des éléments actifs, pastilles monochromes, animations élastiques.",
        "settings.appearance.skins.liquid.description": "Verre liquide façon Apple — matière translucide multi-couches, fond aurora, pastilles, transitions calmes.",
    },
    "de": {
        "settings.appearance.skins.pulse.description": "Neon-Club — OLED-Basis, farbige Halos um aktive Elemente, einfarbige Kapseln, federnde Animationen.",
        "settings.appearance.skins.liquid.description": "Liquid Glass im Apple-Stil — mehrschichtiges transluzentes Material, Aurora-Hintergrund, Kapsel-Elemente, ruhige Bewegung.",
    },
    "es": {
        "settings.appearance.skins.pulse.description": "Club neón — base OLED, halos de color en los elementos activos, cápsulas monocromáticas, animaciones con rebote.",
        "settings.appearance.skins.liquid.description": "Vidrio líquido al estilo Apple — material translúcido multicapa, fondo aurora, cápsulas, movimiento sereno.",
    },
    "it": {
        "settings.appearance.skins.pulse.description": "Club neon — base OLED, aloni colorati attorno agli elementi attivi, capsule mono, animazioni elastiche.",
        "settings.appearance.skins.liquid.description": "Vetro liquido in stile Apple — materiale traslucido multistrato, sfondo aurora, capsule, movimento fluido.",
    },
    "nl": {
        "settings.appearance.skins.liquid.description": "Vloeibaar glas in Apple-stijl — meerlaags doorschijnend materiaal, aurora-achtergrond, capsules, kalme beweging.",
    },
    "pt": {
        "settings.appearance.skins.editorial.description": "Revista em papel — tipografia serifada editorial, linhas finas, grão suave, espaço generoso.",
        "settings.appearance.skins.pulse.description": "Clube néon — base OLED, halos coloridos em torno dos itens ativos, cápsulas mono, animações elásticas.",
        "settings.appearance.skins.liquid.description": "Vidro líquido ao estilo Apple — material translúcido multicamada, fundo aurora, cápsulas, animações suaves.",
    },
    "pt-BR": {
        "settings.appearance.skins.pulse.description": "Clube neon — base OLED, halos coloridos ao redor dos itens ativos, cápsulas mono, animações com efeito de mola.",
        "settings.appearance.skins.liquid.description": "Vidro líquido estilo Apple — material translúcido em multicamadas, fundo aurora, cápsulas arredondadas, movimento calmo.",
    },
    "ru": {
        "settings.appearance.skins.editorial.description": "Журнал на бумаге — журнальная антиква, тонкие разделители, мягкая зернистость, простор.",
        "settings.appearance.skins.pulse.description": "Неоновый клуб — OLED-основа, цветные ореолы вокруг активных элементов, однотонные пилюли, пружинящие анимации.",
    },
    "tr": {
        "settings.appearance.skins.pulse.description": "Neon kulüp — OLED zemin, aktif öğeler etrafında renkli haleler, tek renkli etiketler, yaylı animasyonlar.",
        "settings.appearance.skins.liquid.description": "Apple tarzı sıvı cam — çok katmanlı yarı şeffaf malzeme, aurora arka plan, kapsül etiketler, sakin hareket.",
    },
    "id": {
        "settings.appearance.skins.editorial.description": "Majalah cetak — serif editorial, garis tipis, tekstur halus, ruang lapang.",
        "settings.appearance.skins.pulse.description": "Klub neon — basis OLED, halo warna di sekitar elemen aktif, kapsul mono, animasi membal.",
    },
    "ko": {
        "settings.appearance.skins.pulse.description": "네온 클럽 — OLED 기반, 활성 요소 주변의 컬러 헤일로, 단색 필, 탄력 있는 모션.",
        "settings.appearance.skins.liquid.description": "Apple 스타일 리퀴드 글래스 — 다층 반투명 소재, 오로라 배경, 캡슐형 디자인, 차분한 모션.",
    },
    "zh-CN": {
        # Same CJK half-vs-full-width comma normalization the AI
        # report flagged in zh-CN punctuation.
        "settings.appearance.skins.pulse.description": "霓虹俱乐部 — OLED 底色，活动元素周围的彩色光晕，单色胶囊标签，弹性动效。",
        "settings.appearance.skins.liquid.description": "Apple 风格液态玻璃 — 多层半透明材质，极光背景，胶囊标签，宁静动效。",
    },
    "zh-TW": {
        # AI report flagged a half-width `,` mid-sentence; CJK
        # conventions use the full-width `，` between clauses.
        "settings.appearance.skin.subtitle": "改變密度、表面質感、字體與動效，不只是顏色。",
    },
    "ar": {
        "settings.appearance.skins.editorial.description": "مجلة على ورق — خط سيريف افتتاحي، خطوط فاصلة رفيعة، حبيبات ناعمة، فسحة سخية.",
        "settings.appearance.skins.pulse.description": "نادي نيون — قاعدة OLED، هالات ملوّنة حول العناصر النشطة، شارات كبسولية أحادية، حركات نابضة.",
    },
}


def set_path(data: dict, dotted: str, value: str) -> bool:
    parts = dotted.split(".")
    cursor = data
    for part in parts[:-1]:
        if part not in cursor or not isinstance(cursor[part], dict):
            return False
        cursor = cursor[part]
    leaf = parts[-1]
    if cursor.get(leaf) == value:
        return False
    cursor[leaf] = value
    return True


def patch_locale(path: Path, patches: dict) -> bool:
    with path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)
    changed = False
    for dotted, value in patches.items():
        if set_path(data, dotted, value):
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
    for code, patches in PATCHES.items():
        path = here / f"{code}.json"
        if not path.exists():
            skipped.append(f"{code}(missing)")
            continue
        if patch_locale(path, patches):
            changed.append(code)
        else:
            skipped.append(code)
    sys.stdout.reconfigure(encoding="utf-8")
    print(f"changed ({len(changed)}): {', '.join(changed) or '(none)'}")
    print(f"skipped ({len(skipped)}): {', '.join(skipped) or '(none)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
