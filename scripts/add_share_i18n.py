#!/usr/bin/env python3
"""One-shot injector for the Phase 1.g.3 ShareModal i18n keys.

Adds `playlistView.actions.share` + a top-level `share` block to all
17 locale files. Translations are inline rather than fetched from a
service so a reviewer can audit them in the PR.

Run from repo root:
    bun run python scripts/add_share_i18n.py
"""

from __future__ import annotations

import json
from pathlib import Path

# (locale code, action_share label, top-level share block)
TRANSLATIONS: dict[str, tuple[str, dict[str, str]]] = {
    "fr": (
        "Partager",
        {
            "title": "Partager la playlist",
            "subtitle": "Générez un lien public pour « {{name}} ». Tout le monde pourra le consulter dans un navigateur.",
            "idleHint": "Le lien sera actif jusqu'à ce que vous le révoquiez. Vous pourrez toujours le désactiver depuis cette fenêtre.",
            "mint": "Générer le lien",
            "activeHint": "Lien actif sur tous vos appareils.",
            "revoke": "Désactiver",
            "qrAlt": "QR code du lien partagé",
            "urlLabel": "URL publique",
            "copy": "Copier",
            "copied": "Copié",
            "close": "Fermer",
            "errorClipboard": "Impossible d'accéder au presse-papier. Sélectionnez l'URL pour la copier manuellement.",
        },
    ),
    "en": (
        "Share",
        {
            "title": "Share playlist",
            "subtitle": "Generate a public link for “{{name}}”. Anyone can open it in a browser.",
            "idleHint": "The link stays active until you revoke it. You can disable it at any time from this dialog.",
            "mint": "Generate link",
            "activeHint": "Link is active on all your devices.",
            "revoke": "Disable",
            "qrAlt": "QR code for the shared link",
            "urlLabel": "Public URL",
            "copy": "Copy",
            "copied": "Copied",
            "close": "Close",
            "errorClipboard": "Could not access the clipboard. Select the URL above to copy it manually.",
        },
    ),
    "es": (
        "Compartir",
        {
            "title": "Compartir la lista",
            "subtitle": "Genera un enlace público para «{{name}}». Cualquiera podrá abrirlo en un navegador.",
            "idleHint": "El enlace permanecerá activo hasta que lo revoques. Puedes desactivarlo en cualquier momento desde esta ventana.",
            "mint": "Generar enlace",
            "activeHint": "Enlace activo en todos tus dispositivos.",
            "revoke": "Desactivar",
            "qrAlt": "Código QR del enlace compartido",
            "urlLabel": "URL pública",
            "copy": "Copiar",
            "copied": "Copiado",
            "close": "Cerrar",
            "errorClipboard": "No se pudo acceder al portapapeles. Selecciona la URL para copiarla manualmente.",
        },
    ),
    "de": (
        "Teilen",
        {
            "title": "Playlist teilen",
            "subtitle": "Erstelle einen öffentlichen Link für „{{name}}“. Jeder kann ihn im Browser öffnen.",
            "idleHint": "Der Link bleibt aktiv, bis du ihn widerrufst. Du kannst ihn jederzeit in diesem Fenster deaktivieren.",
            "mint": "Link erstellen",
            "activeHint": "Link auf all deinen Geräten aktiv.",
            "revoke": "Deaktivieren",
            "qrAlt": "QR-Code des geteilten Links",
            "urlLabel": "Öffentliche URL",
            "copy": "Kopieren",
            "copied": "Kopiert",
            "close": "Schließen",
            "errorClipboard": "Zugriff auf die Zwischenablage nicht möglich. Markiere die URL, um sie manuell zu kopieren.",
        },
    ),
    "it": (
        "Condividi",
        {
            "title": "Condividi la playlist",
            "subtitle": "Genera un link pubblico per «{{name}}». Chiunque potrà aprirlo in un browser.",
            "idleHint": "Il link rimarrà attivo finché non lo revochi. Puoi disattivarlo in qualsiasi momento da questa finestra.",
            "mint": "Genera link",
            "activeHint": "Link attivo su tutti i tuoi dispositivi.",
            "revoke": "Disattiva",
            "qrAlt": "Codice QR del link condiviso",
            "urlLabel": "URL pubblico",
            "copy": "Copia",
            "copied": "Copiato",
            "close": "Chiudi",
            "errorClipboard": "Impossibile accedere agli appunti. Seleziona l'URL per copiarlo manualmente.",
        },
    ),
    "nl": (
        "Delen",
        {
            "title": "Afspeellijst delen",
            "subtitle": "Genereer een openbare link voor “{{name}}”. Iedereen kan deze in een browser openen.",
            "idleHint": "De link blijft actief totdat je hem intrekt. Je kunt hem op elk moment via dit venster uitschakelen.",
            "mint": "Link genereren",
            "activeHint": "Link is actief op al je apparaten.",
            "revoke": "Uitschakelen",
            "qrAlt": "QR-code voor de gedeelde link",
            "urlLabel": "Openbare URL",
            "copy": "Kopiëren",
            "copied": "Gekopieerd",
            "close": "Sluiten",
            "errorClipboard": "Geen toegang tot het klembord. Selecteer de URL om deze handmatig te kopiëren.",
        },
    ),
    "pt": (
        "Partilhar",
        {
            "title": "Partilhar a playlist",
            "subtitle": "Gere uma ligação pública para “{{name}}”. Qualquer pessoa poderá abri-la num navegador.",
            "idleHint": "A ligação permanecerá ativa até a revogar. Pode desativá-la a qualquer momento nesta janela.",
            "mint": "Gerar ligação",
            "activeHint": "Ligação ativa em todos os seus dispositivos.",
            "revoke": "Desativar",
            "qrAlt": "Código QR da ligação partilhada",
            "urlLabel": "URL pública",
            "copy": "Copiar",
            "copied": "Copiado",
            "close": "Fechar",
            "errorClipboard": "Não foi possível aceder à área de transferência. Selecione o URL para o copiar manualmente.",
        },
    ),
    "pt-BR": (
        "Compartilhar",
        {
            "title": "Compartilhar a playlist",
            "subtitle": "Gere um link público para “{{name}}”. Qualquer pessoa poderá abri-lo em um navegador.",
            "idleHint": "O link permanecerá ativo até você revogá-lo. Você pode desativá-lo a qualquer momento nesta janela.",
            "mint": "Gerar link",
            "activeHint": "Link ativo em todos os seus dispositivos.",
            "revoke": "Desativar",
            "qrAlt": "QR code do link compartilhado",
            "urlLabel": "URL pública",
            "copy": "Copiar",
            "copied": "Copiado",
            "close": "Fechar",
            "errorClipboard": "Não foi possível acessar a área de transferência. Selecione a URL para copiá-la manualmente.",
        },
    ),
    "ru": (
        "Поделиться",
        {
            "title": "Поделиться плейлистом",
            "subtitle": "Создайте публичную ссылку на «{{name}}». Любой сможет открыть её в браузере.",
            "idleHint": "Ссылка будет активна, пока вы её не отзовёте. Её можно отключить в этом окне в любой момент.",
            "mint": "Создать ссылку",
            "activeHint": "Ссылка активна на всех ваших устройствах.",
            "revoke": "Отключить",
            "qrAlt": "QR-код общей ссылки",
            "urlLabel": "Публичная ссылка",
            "copy": "Копировать",
            "copied": "Скопировано",
            "close": "Закрыть",
            "errorClipboard": "Нет доступа к буферу обмена. Выделите ссылку и скопируйте вручную.",
        },
    ),
    "tr": (
        "Paylaş",
        {
            "title": "Çalma listesini paylaş",
            "subtitle": "“{{name}}” için herkese açık bir bağlantı oluşturun. Herkes tarayıcıda açabilir.",
            "idleHint": "Bağlantı, iptal edene kadar aktif kalır. Bu pencereden istediğiniz zaman devre dışı bırakabilirsiniz.",
            "mint": "Bağlantı oluştur",
            "activeHint": "Bağlantı tüm cihazlarınızda aktif.",
            "revoke": "Devre dışı bırak",
            "qrAlt": "Paylaşılan bağlantının QR kodu",
            "urlLabel": "Herkese açık URL",
            "copy": "Kopyala",
            "copied": "Kopyalandı",
            "close": "Kapat",
            "errorClipboard": "Panoya erişilemiyor. URL'yi seçerek manuel olarak kopyalayın.",
        },
    ),
    "id": (
        "Bagikan",
        {
            "title": "Bagikan playlist",
            "subtitle": "Buat tautan publik untuk “{{name}}”. Siapa pun dapat membukanya di peramban.",
            "idleHint": "Tautan tetap aktif sampai Anda mencabutnya. Anda dapat menonaktifkannya kapan saja dari jendela ini.",
            "mint": "Buat tautan",
            "activeHint": "Tautan aktif di semua perangkat Anda.",
            "revoke": "Nonaktifkan",
            "qrAlt": "Kode QR untuk tautan yang dibagikan",
            "urlLabel": "URL publik",
            "copy": "Salin",
            "copied": "Tersalin",
            "close": "Tutup",
            "errorClipboard": "Tidak dapat mengakses papan klip. Pilih URL untuk menyalinnya secara manual.",
        },
    ),
    "ja": (
        "共有",
        {
            "title": "プレイリストを共有",
            "subtitle": "「{{name}}」の公開リンクを生成します。誰でもブラウザで開けます。",
            "idleHint": "リンクは無効化するまで有効です。このウィンドウからいつでも無効にできます。",
            "mint": "リンクを生成",
            "activeHint": "リンクはすべての端末でアクティブです。",
            "revoke": "無効化",
            "qrAlt": "共有リンクの QR コード",
            "urlLabel": "公開 URL",
            "copy": "コピー",
            "copied": "コピーしました",
            "close": "閉じる",
            "errorClipboard": "クリップボードにアクセスできません。URL を選択して手動でコピーしてください。",
        },
    ),
    "ko": (
        "공유",
        {
            "title": "재생목록 공유",
            "subtitle": "“{{name}}”의 공개 링크를 생성합니다. 누구나 브라우저에서 열 수 있습니다.",
            "idleHint": "링크는 해제할 때까지 활성 상태로 유지됩니다. 이 창에서 언제든지 비활성화할 수 있습니다.",
            "mint": "링크 생성",
            "activeHint": "모든 기기에서 링크가 활성 상태입니다.",
            "revoke": "비활성화",
            "qrAlt": "공유 링크의 QR 코드",
            "urlLabel": "공개 URL",
            "copy": "복사",
            "copied": "복사됨",
            "close": "닫기",
            "errorClipboard": "클립보드에 액세스할 수 없습니다. URL을 선택해 수동으로 복사하세요.",
        },
    ),
    "zh-CN": (
        "分享",
        {
            "title": "分享播放列表",
            "subtitle": "为“{{name}}”生成公开链接。任何人都可以在浏览器中打开。",
            "idleHint": "链接会一直有效，直到你撤销它。可以随时在此窗口中停用。",
            "mint": "生成链接",
            "activeHint": "链接在你所有设备上均已激活。",
            "revoke": "停用",
            "qrAlt": "分享链接的二维码",
            "urlLabel": "公开 URL",
            "copy": "复制",
            "copied": "已复制",
            "close": "关闭",
            "errorClipboard": "无法访问剪贴板。请选中 URL 手动复制。",
        },
    ),
    "zh-TW": (
        "分享",
        {
            "title": "分享播放清單",
            "subtitle": "為「{{name}}」產生公開連結。任何人都可以在瀏覽器中開啟。",
            "idleHint": "連結會持續有效，直到您撤銷為止。可隨時在此視窗中停用。",
            "mint": "產生連結",
            "activeHint": "連結在您所有裝置上皆已啟用。",
            "revoke": "停用",
            "qrAlt": "分享連結的 QR code",
            "urlLabel": "公開 URL",
            "copy": "複製",
            "copied": "已複製",
            "close": "關閉",
            "errorClipboard": "無法存取剪貼簿。請選取 URL 手動複製。",
        },
    ),
    "ar": (
        "مشاركة",
        {
            "title": "مشاركة قائمة التشغيل",
            "subtitle": "أنشئ رابطاً عاماً لـ «{{name}}». يمكن لأي شخص فتحه في المتصفح.",
            "idleHint": "يظل الرابط مفعّلاً حتى تقوم بإلغائه. يمكنك تعطيله في أي وقت من هذه النافذة.",
            "mint": "إنشاء الرابط",
            "activeHint": "الرابط مفعّل على جميع أجهزتك.",
            "revoke": "تعطيل",
            "qrAlt": "رمز QR للرابط المشترك",
            "urlLabel": "الرابط العام",
            "copy": "نسخ",
            "copied": "تم النسخ",
            "close": "إغلاق",
            "errorClipboard": "تعذّر الوصول إلى الحافظة. حدّد الرابط لنسخه يدوياً.",
        },
    ),
    "hi": (
        "साझा करें",
        {
            "title": "प्लेलिस्ट साझा करें",
            "subtitle": "“{{name}}” के लिए सार्वजनिक लिंक बनाएँ। कोई भी इसे ब्राउज़र में खोल सकता है।",
            "idleHint": "लिंक तब तक सक्रिय रहेगा जब तक आप इसे रद्द नहीं करते। आप इसे कभी भी इस विंडो से अक्षम कर सकते हैं।",
            "mint": "लिंक बनाएँ",
            "activeHint": "लिंक आपके सभी डिवाइसों पर सक्रिय है।",
            "revoke": "अक्षम करें",
            "qrAlt": "साझा लिंक का QR कोड",
            "urlLabel": "सार्वजनिक URL",
            "copy": "कॉपी",
            "copied": "कॉपी हो गया",
            "close": "बंद करें",
            "errorClipboard": "क्लिपबोर्ड तक पहुँच नहीं हुई। URL को चुनकर मैन्युअल रूप से कॉपी करें।",
        },
    ),
}


def patch_locale(path: Path, action_label: str, share_block: dict[str, str]) -> bool:
    with path.open(encoding="utf-8") as f:
        data = json.load(f)

    changed = False

    # 1. playlistView.actions.share
    playlist_view = data.get("playlistView")
    if isinstance(playlist_view, dict):
        actions = playlist_view.setdefault("actions", {})
        if actions.get("share") != action_label:
            actions["share"] = action_label
            changed = True

    # 2. Top-level "share" block. Coexists with the historical
    # "share" namespace (Now Playing / Wrapped card export) if it
    # already exists — those entries get merged in.
    existing_share = data.get("share")
    if not isinstance(existing_share, dict):
        existing_share = {}
        data["share"] = existing_share
    for key, value in share_block.items():
        if existing_share.get(key) != value:
            existing_share[key] = value
            changed = True

    if changed:
        with path.open("w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
            f.write("\n")
    return changed


def main() -> int:
    root = Path(__file__).resolve().parent.parent / "src" / "i18n" / "locales"
    missing = [
        locale for locale in TRANSLATIONS if not (root / f"{locale}.json").exists()
    ]
    if missing:
        # Fail-fast: silently skipping a locale would let a PR land
        # with incomplete translations and only surface as
        # missing-key warnings at runtime. Stop with a non-zero
        # status so CI catches the gap before merge.
        raise FileNotFoundError(
            "Missing locale file(s): " + ", ".join(f"{m}.json" for m in missing)
        )

    total = 0
    for locale, (label, block) in TRANSLATIONS.items():
        path = root / f"{locale}.json"
        if patch_locale(path, label, block):
            print(f"  patched: {path.name}")
            total += 1
        else:
            print(f"  noop:    {path.name}")
    print(f"\n{total} locale file(s) updated.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
