"""One-shot script: propagate i18n strings introduced by the
"lyrics save destination" PR (issue #201 follow-up).

Adds:
  - `lyricsEditor.destinationLabel`
  - `lyricsEditor.destination.{tag,sidecar,db_only}.{label,hint}`
  - `lyrics.toast.sidecarWriteSkipped`
  - `settings.lyricsDestination.{title,subtitle}`
  - `onboarding.lyrics.{title,description,hint}`

across all 17 locales. Idempotent — re-running on a locale that
already has the keys is a no-op.

Run from repo root:
    python scripts/add_lyrics_destination_i18n.py
"""

import json
import sys
from pathlib import Path


TRANSLATIONS = {
    "fr": {
        "destinationLabel": "Destination par défaut des paroles",
        "tag_label": "Tag intégré",
        "tag_hint": "Écrit dans l'USLT/©lyr du fichier audio (compatible foobar2000, iTunes…).",
        "sidecar_label": "Fichier sidecar",
        "sidecar_hint": "Écrit un .lrc / .txt à côté du fichier audio. Tags du fichier intacts.",
        "db_only_label": "Base WaveFlow uniquement",
        "db_only_hint": "Reste dans le cache de l'app. Aucun fichier ni tag touché.",
        "sidecarWriteSkipped": "Paroles enregistrées dans la base, mais le format TTML ne tient pas dans un fichier .lrc/.txt.",
        "settings_title": "Destination des paroles éditées",
        "settings_subtitle": "Où sauvegarder les paroles que vous saisissez dans l'éditeur. Vous pouvez surcharger ce choix à chaque édition.",
        "onb_title": "Où ranger les paroles ?",
        "onb_description": "Quand vous éditez des paroles, où voulez-vous qu'elles atterrissent ? Vous pourrez changer plus tard dans Réglages → Lecture.",
        "onb_hint": "Choisissez « Fichier sidecar » pour garder vos fichiers audio inchangés (ce que font foobar2000 et compagnie).",
    },
    "en": {
        "destinationLabel": "Default lyrics destination",
        "tag_label": "Embedded tag",
        "tag_hint": "Writes USLT/©lyr in the audio file (foobar2000, iTunes… compatible).",
        "sidecar_label": "Sidecar file",
        "sidecar_hint": "Writes a .lrc / .txt next to the audio file. Audio file tags untouched.",
        "db_only_label": "WaveFlow database only",
        "db_only_hint": "Stays in the app cache. No files or tags are touched.",
        "sidecarWriteSkipped": "Lyrics saved to the database, but TTML can't ride a .lrc/.txt sidecar.",
        "settings_title": "Saved-lyrics destination",
        "settings_subtitle": "Where lyrics typed into the editor land. You can override this per-edit from the editor footer.",
        "onb_title": "Where should lyrics live?",
        "onb_description": "When you edit lyrics, where should they go? You can change this anytime from Settings → Playback.",
        "onb_hint": "Pick \"Sidecar file\" to keep your audio files untouched — what foobar2000 and friends do.",
    },
    "es": {
        "destinationLabel": "Destino predeterminado de las letras",
        "tag_label": "Etiqueta integrada",
        "tag_hint": "Escribe en USLT/©lyr del archivo de audio (compatible con foobar2000, iTunes…).",
        "sidecar_label": "Archivo sidecar",
        "sidecar_hint": "Escribe un .lrc / .txt junto al archivo de audio. Etiquetas intactas.",
        "db_only_label": "Solo base de datos de WaveFlow",
        "db_only_hint": "Se queda en la caché de la app. No se tocan archivos ni etiquetas.",
        "sidecarWriteSkipped": "Letras guardadas en la base, pero TTML no cabe en un archivo .lrc/.txt.",
        "settings_title": "Destino de las letras guardadas",
        "settings_subtitle": "Dónde se guardan las letras que escribes en el editor. Puedes anularlo en cada edición.",
        "onb_title": "¿Dónde guardar las letras?",
        "onb_description": "Cuando editas letras, ¿dónde quieres que se guarden? Podrás cambiarlo en Ajustes → Reproducción.",
        "onb_hint": "Elige \"Archivo sidecar\" para mantener tus archivos de audio intactos (lo que hacen foobar2000 y compañía).",
    },
    "de": {
        "destinationLabel": "Standard-Speicherort für Liedtexte",
        "tag_label": "Eingebettetes Tag",
        "tag_hint": "Schreibt USLT/©lyr in die Audiodatei (kompatibel mit foobar2000, iTunes…).",
        "sidecar_label": "Sidecar-Datei",
        "sidecar_hint": "Schreibt eine .lrc / .txt neben der Audiodatei. Tags bleiben unberührt.",
        "db_only_label": "Nur WaveFlow-Datenbank",
        "db_only_hint": "Bleibt im App-Cache. Keine Dateien oder Tags werden verändert.",
        "sidecarWriteSkipped": "Liedtext in der Datenbank gespeichert, aber TTML passt nicht in eine .lrc/.txt-Datei.",
        "settings_title": "Speicherort für bearbeitete Liedtexte",
        "settings_subtitle": "Wo Liedtexte aus dem Editor landen. Bei jeder Bearbeitung überschreibbar.",
        "onb_title": "Wo sollen Liedtexte landen?",
        "onb_description": "Wenn Sie Liedtexte bearbeiten, wo sollen sie hin? Änderbar unter Einstellungen → Wiedergabe.",
        "onb_hint": "Wählen Sie \"Sidecar-Datei\", um Audiodateien unberührt zu lassen — wie foobar2000 & Co.",
    },
    "it": {
        "destinationLabel": "Destinazione predefinita dei testi",
        "tag_label": "Tag integrato",
        "tag_hint": "Scrive USLT/©lyr nel file audio (compatibile con foobar2000, iTunes…).",
        "sidecar_label": "File sidecar",
        "sidecar_hint": "Scrive un .lrc / .txt accanto al file audio. Tag intatti.",
        "db_only_label": "Solo database WaveFlow",
        "db_only_hint": "Rimane nella cache dell'app. Nessun file o tag toccato.",
        "sidecarWriteSkipped": "Testi salvati nel database, ma TTML non entra in un file .lrc/.txt.",
        "settings_title": "Destinazione dei testi salvati",
        "settings_subtitle": "Dove vanno i testi scritti nell'editor. Puoi sovrascrivere a ogni modifica.",
        "onb_title": "Dove salvare i testi?",
        "onb_description": "Quando modifichi i testi, dove devono andare? Modificabile in Impostazioni → Riproduzione.",
        "onb_hint": "Scegli \"File sidecar\" per non toccare i file audio — come fanno foobar2000 e simili.",
    },
    "nl": {
        "destinationLabel": "Standaardbestemming voor songteksten",
        "tag_label": "Ingesloten tag",
        "tag_hint": "Schrijft USLT/©lyr in het audiobestand (compatibel met foobar2000, iTunes…).",
        "sidecar_label": "Sidecar-bestand",
        "sidecar_hint": "Schrijft een .lrc / .txt naast het audiobestand. Tags blijven onaangeroerd.",
        "db_only_label": "Alleen WaveFlow-database",
        "db_only_hint": "Blijft in de cache van de app. Geen bestanden of tags worden aangeraakt.",
        "sidecarWriteSkipped": "Songtekst opgeslagen in de database, maar TTML past niet in een .lrc/.txt-bestand.",
        "settings_title": "Bestemming voor opgeslagen songteksten",
        "settings_subtitle": "Waar songteksten uit de editor heen gaan. Per bewerking te overschrijven.",
        "onb_title": "Waar moeten songteksten heen?",
        "onb_description": "Waar moeten songteksten heen die je bewerkt? Aanpasbaar via Instellingen → Afspelen.",
        "onb_hint": "Kies \"Sidecar-bestand\" om je audiobestanden ongemoeid te laten — zoals foobar2000 & co.",
    },
    "pt": {
        "destinationLabel": "Destino predefinido das letras",
        "tag_label": "Tag integrada",
        "tag_hint": "Escreve USLT/©lyr no ficheiro de áudio (compatível com foobar2000, iTunes…).",
        "sidecar_label": "Ficheiro sidecar",
        "sidecar_hint": "Escreve um .lrc / .txt junto do ficheiro de áudio. Tags intactas.",
        "db_only_label": "Apenas base de dados WaveFlow",
        "db_only_hint": "Permanece na cache da app. Nenhum ficheiro nem tag são tocados.",
        "sidecarWriteSkipped": "Letras guardadas na base de dados, mas TTML não cabe num ficheiro .lrc/.txt.",
        "settings_title": "Destino das letras guardadas",
        "settings_subtitle": "Para onde vão as letras escritas no editor. Pode ser substituído a cada edição.",
        "onb_title": "Onde guardar as letras?",
        "onb_description": "Quando edita letras, para onde devem ir? Pode alterar em Definições → Reprodução.",
        "onb_hint": "Escolha \"Ficheiro sidecar\" para manter os seus ficheiros de áudio intactos — como o foobar2000.",
    },
    "pt-BR": {
        "destinationLabel": "Destino padrão das letras",
        "tag_label": "Tag integrada",
        "tag_hint": "Escreve USLT/©lyr no arquivo de áudio (compatível com foobar2000, iTunes…).",
        "sidecar_label": "Arquivo sidecar",
        "sidecar_hint": "Escreve um .lrc / .txt ao lado do arquivo de áudio. Tags intactas.",
        "db_only_label": "Apenas banco WaveFlow",
        "db_only_hint": "Fica no cache do app. Nenhum arquivo ou tag é tocado.",
        "sidecarWriteSkipped": "Letras salvas no banco, mas TTML não cabe em um arquivo .lrc/.txt.",
        "settings_title": "Destino das letras salvas",
        "settings_subtitle": "Para onde vão as letras digitadas no editor. Pode ser substituído por edição.",
        "onb_title": "Onde salvar as letras?",
        "onb_description": "Quando você edita letras, para onde elas devem ir? Pode mudar em Configurações → Reprodução.",
        "onb_hint": "Escolha \"Arquivo sidecar\" para manter seus arquivos de áudio intactos — como faz o foobar2000.",
    },
    "ru": {
        "destinationLabel": "Куда сохранять тексты песен по умолчанию",
        "tag_label": "Встроенный тег",
        "tag_hint": "Записывает USLT/©lyr в аудиофайл (совместимо с foobar2000, iTunes…).",
        "sidecar_label": "Файл-спутник",
        "sidecar_hint": "Пишет .lrc / .txt рядом с аудиофайлом. Теги файла не трогаются.",
        "db_only_label": "Только база WaveFlow",
        "db_only_hint": "Остаётся в кэше приложения. Файлы и теги не затрагиваются.",
        "sidecarWriteSkipped": "Текст сохранён в базе, но TTML не помещается в файл .lrc/.txt.",
        "settings_title": "Назначение сохранённых текстов",
        "settings_subtitle": "Куда идут тексты из редактора. Можно переопределить на каждое сохранение.",
        "onb_title": "Где хранить тексты?",
        "onb_description": "Куда должны попадать отредактированные тексты? Можно изменить в Настройки → Воспроизведение.",
        "onb_hint": "Выберите «Файл-спутник», чтобы не трогать аудиофайлы — как делают foobar2000 и компания.",
    },
    "tr": {
        "destinationLabel": "Varsayılan şarkı sözü hedefi",
        "tag_label": "Gömülü etiket",
        "tag_hint": "Ses dosyasının USLT/©lyr alanına yazar (foobar2000, iTunes uyumlu).",
        "sidecar_label": "Sidecar dosyası",
        "sidecar_hint": "Ses dosyasının yanına .lrc / .txt yazar. Dosya etiketlerine dokunulmaz.",
        "db_only_label": "Yalnızca WaveFlow veritabanı",
        "db_only_hint": "Uygulama önbelleğinde kalır. Hiçbir dosya veya etikete dokunulmaz.",
        "sidecarWriteSkipped": "Şarkı sözleri veritabanına kaydedildi, ancak TTML bir .lrc/.txt dosyasına sığmaz.",
        "settings_title": "Kayıtlı şarkı sözü hedefi",
        "settings_subtitle": "Düzenleyiciden gelen şarkı sözlerinin gideceği yer. Her düzenlemede geçersiz kılınabilir.",
        "onb_title": "Şarkı sözleri nereye gitsin?",
        "onb_description": "Şarkı sözlerini düzenlediğinizde nereye gitsinler? Ayarlar → Oynatma'dan değiştirebilirsiniz.",
        "onb_hint": "Ses dosyalarınızı bozulmadan tutmak için \"Sidecar dosyası\" seçin — foobar2000 ve benzerleri böyle yapar.",
    },
    "id": {
        "destinationLabel": "Tujuan default lirik",
        "tag_label": "Tag tertanam",
        "tag_hint": "Menulis USLT/©lyr di file audio (kompatibel dengan foobar2000, iTunes…).",
        "sidecar_label": "File sidecar",
        "sidecar_hint": "Menulis .lrc / .txt di samping file audio. Tag file tidak disentuh.",
        "db_only_label": "Hanya database WaveFlow",
        "db_only_hint": "Tetap di cache app. Tidak ada file atau tag yang disentuh.",
        "sidecarWriteSkipped": "Lirik disimpan ke database, tetapi TTML tidak muat di file .lrc/.txt.",
        "settings_title": "Tujuan lirik tersimpan",
        "settings_subtitle": "Tempat lirik dari editor disimpan. Dapat di-override per pengeditan.",
        "onb_title": "Di mana menyimpan lirik?",
        "onb_description": "Ketika Anda mengedit lirik, ke mana mereka harus pergi? Dapat diubah di Pengaturan → Pemutaran.",
        "onb_hint": "Pilih \"File sidecar\" agar file audio Anda tetap utuh — seperti yang dilakukan foobar2000 dkk.",
    },
    "ja": {
        "destinationLabel": "歌詞の既定の保存先",
        "tag_label": "埋め込みタグ",
        "tag_hint": "オーディオファイルのUSLT/©lyrに書き込みます（foobar2000、iTunesと互換）。",
        "sidecar_label": "サイドカーファイル",
        "sidecar_hint": "オーディオファイルの隣に .lrc / .txt を書き込みます。タグはそのまま。",
        "db_only_label": "WaveFlow のデータベースのみ",
        "db_only_hint": "アプリのキャッシュに残ります。ファイルもタグも変更しません。",
        "sidecarWriteSkipped": "歌詞はデータベースに保存しましたが、TTMLは .lrc/.txt ファイルに収まりません。",
        "settings_title": "保存される歌詞の保存先",
        "settings_subtitle": "エディターで入力した歌詞の保存先。編集ごとに上書き可能。",
        "onb_title": "歌詞はどこに保存しますか？",
        "onb_description": "歌詞を編集したとき、どこに保存しますか？設定 → 再生 でいつでも変更できます。",
        "onb_hint": "オーディオファイルを変更したくない場合は「サイドカーファイル」を選択 — foobar2000などと同じ方式。",
    },
    "ko": {
        "destinationLabel": "기본 가사 저장 위치",
        "tag_label": "내장 태그",
        "tag_hint": "오디오 파일의 USLT/©lyr에 씁니다 (foobar2000, iTunes 호환).",
        "sidecar_label": "사이드카 파일",
        "sidecar_hint": "오디오 파일 옆에 .lrc / .txt를 씁니다. 파일 태그는 그대로.",
        "db_only_label": "WaveFlow 데이터베이스만",
        "db_only_hint": "앱 캐시에 남습니다. 파일이나 태그를 건드리지 않습니다.",
        "sidecarWriteSkipped": "가사를 데이터베이스에 저장했지만 TTML은 .lrc/.txt 파일에 맞지 않습니다.",
        "settings_title": "저장되는 가사의 위치",
        "settings_subtitle": "에디터에서 입력한 가사가 저장되는 위치. 편집할 때마다 재정의 가능.",
        "onb_title": "가사를 어디에 저장할까요?",
        "onb_description": "가사를 편집할 때 어디에 저장하시겠습니까? 설정 → 재생에서 언제든 변경할 수 있습니다.",
        "onb_hint": "오디오 파일을 그대로 유지하려면 \"사이드카 파일\"을 선택하세요 — foobar2000 등이 하는 방식입니다.",
    },
    "zh-CN": {
        "destinationLabel": "歌词默认保存位置",
        "tag_label": "嵌入标签",
        "tag_hint": "写入音频文件的 USLT/©lyr（兼容 foobar2000、iTunes…）。",
        "sidecar_label": "Sidecar 文件",
        "sidecar_hint": "在音频文件旁边写入 .lrc / .txt。文件标签不受影响。",
        "db_only_label": "仅 WaveFlow 数据库",
        "db_only_hint": "保留在应用缓存中。不会触碰任何文件或标签。",
        "sidecarWriteSkipped": "歌词已保存到数据库,但 TTML 无法放入 .lrc/.txt 文件。",
        "settings_title": "已保存歌词的位置",
        "settings_subtitle": "编辑器中输入的歌词将保存到何处。可以在每次编辑时覆盖。",
        "onb_title": "歌词保存在哪里?",
        "onb_description": "编辑歌词时,它们应该保存到何处?可在 设置 → 播放 中随时更改。",
        "onb_hint": "选择「Sidecar 文件」可让音频文件保持不变 — foobar2000 等播放器就是这样做的。",
    },
    "zh-TW": {
        "destinationLabel": "歌詞預設儲存位置",
        "tag_label": "嵌入標籤",
        "tag_hint": "寫入音訊檔的 USLT/©lyr(相容於 foobar2000、iTunes…)。",
        "sidecar_label": "Sidecar 檔案",
        "sidecar_hint": "在音訊檔旁邊寫入 .lrc / .txt。檔案標籤不受影響。",
        "db_only_label": "僅 WaveFlow 資料庫",
        "db_only_hint": "留在應用快取中。不會碰任何檔案或標籤。",
        "sidecarWriteSkipped": "歌詞已儲存到資料庫,但 TTML 無法放入 .lrc/.txt 檔案。",
        "settings_title": "已儲存歌詞的位置",
        "settings_subtitle": "編輯器中輸入的歌詞會儲存到何處。可在每次編輯時覆寫。",
        "onb_title": "歌詞要儲存在哪裡?",
        "onb_description": "編輯歌詞時,它們應該儲存到何處?可在 設定 → 播放 中隨時變更。",
        "onb_hint": "選擇「Sidecar 檔案」可讓音訊檔保持不變 — foobar2000 等播放器就是這樣做的。",
    },
    "ar": {
        "destinationLabel": "وجهة الكلمات الافتراضية",
        "tag_label": "علامة مدمجة",
        "tag_hint": "تكتب USLT/©lyr في الملف الصوتي (متوافق مع foobar2000 وiTunes…).",
        "sidecar_label": "ملف Sidecar",
        "sidecar_hint": "يكتب .lrc / .txt بجانب الملف الصوتي. لا تُمَس علامات الملف.",
        "db_only_label": "قاعدة بيانات WaveFlow فقط",
        "db_only_hint": "تبقى في ذاكرة التخزين المؤقت للتطبيق. لا تُلمس أي ملفات أو علامات.",
        "sidecarWriteSkipped": "تم حفظ الكلمات في قاعدة البيانات، لكن TTML لا يتسع في ملف .lrc/.txt.",
        "settings_title": "وجهة الكلمات المحفوظة",
        "settings_subtitle": "أين تذهب الكلمات المكتوبة في المحرر. يمكن تجاوز ذلك في كل تعديل.",
        "onb_title": "أين تُحفظ الكلمات؟",
        "onb_description": "عند تعديل الكلمات، أين يجب أن تذهب؟ يمكن تغيير ذلك من الإعدادات → التشغيل.",
        "onb_hint": "اختر \"ملف Sidecar\" للحفاظ على ملفاتك الصوتية كما هي — هذا ما يفعله foobar2000 وأمثاله.",
    },
    "hi": {
        "destinationLabel": "गीत के बोल का डिफ़ॉल्ट गंतव्य",
        "tag_label": "एम्बेडेड टैग",
        "tag_hint": "ऑडियो फ़ाइल के USLT/©lyr में लिखता है (foobar2000, iTunes संगत).",
        "sidecar_label": "साइडकार फ़ाइल",
        "sidecar_hint": "ऑडियो फ़ाइल के बगल में .lrc / .txt लिखता है. फ़ाइल टैग अछूते रहते हैं.",
        "db_only_label": "केवल WaveFlow डेटाबेस",
        "db_only_hint": "ऐप कैश में रहता है. कोई फ़ाइल या टैग नहीं छुआ जाता.",
        "sidecarWriteSkipped": "गीत डेटाबेस में सहेजे गए, लेकिन TTML .lrc/.txt फ़ाइल में फिट नहीं होता.",
        "settings_title": "सहेजे गए गीतों का गंतव्य",
        "settings_subtitle": "एडिटर में टाइप किए गए गीत कहाँ जाते हैं. प्रत्येक संपादन पर ओवरराइड संभव.",
        "onb_title": "गीत के बोल कहाँ रहें?",
        "onb_description": "जब आप गीत संपादित करते हैं, तो वे कहाँ जाने चाहिए? सेटिंग्स → प्लेबैक में कभी भी बदल सकते हैं.",
        "onb_hint": "अपनी ऑडियो फ़ाइलों को अछूता रखने के लिए \"साइडकार फ़ाइल\" चुनें — foobar2000 जैसे प्लेयर यही करते हैं.",
    },
}


def patch_locale(path: Path, tr: dict) -> bool:
    with path.open("r", encoding="utf-8") as fh:
        data = json.load(fh)

    changed = False

    # lyricsEditor.destinationLabel + lyricsEditor.destination.<id>.{label,hint}
    le = data.setdefault("lyricsEditor", {})
    if "destinationLabel" not in le:
        le["destinationLabel"] = tr["destinationLabel"]
        changed = True
    dest_block = le.setdefault("destination", {})
    for key, label_key, hint_key in [
        ("tag", "tag_label", "tag_hint"),
        ("sidecar", "sidecar_label", "sidecar_hint"),
        ("db_only", "db_only_label", "db_only_hint"),
    ]:
        sub = dest_block.setdefault(key, {})
        if "label" not in sub:
            sub["label"] = tr[label_key]
            changed = True
        if "hint" not in sub:
            sub["hint"] = tr[hint_key]
            changed = True

    # lyrics.toast.sidecarWriteSkipped
    lyrics = data.setdefault("lyrics", {})
    toast = lyrics.setdefault("toast", {})
    if "sidecarWriteSkipped" not in toast:
        toast["sidecarWriteSkipped"] = tr["sidecarWriteSkipped"]
        changed = True

    # settings.lyricsDestination.{title,subtitle}
    settings = data.setdefault("settings", {})
    sd = settings.setdefault("lyricsDestination", {})
    if "title" not in sd:
        sd["title"] = tr["settings_title"]
        changed = True
    if "subtitle" not in sd:
        sd["subtitle"] = tr["settings_subtitle"]
        changed = True

    # onboarding.lyrics.{title,description,hint}
    onb = data.setdefault("onboarding", {})
    block = onb.setdefault("lyrics", {})
    if "title" not in block:
        block["title"] = tr["onb_title"]
        changed = True
    if "description" not in block:
        block["description"] = tr["onb_description"]
        changed = True
    if "hint" not in block:
        block["hint"] = tr["onb_hint"]
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
    for code, tr in TRANSLATIONS.items():
        path = here / f"{code}.json"
        if not path.exists():
            print(f"  skip {code} — file missing")
            skipped.append(code)
            continue
        if patch_locale(path, tr):
            changed.append(code)
        else:
            skipped.append(code)
    print(f"changed ({len(changed)}): {', '.join(changed) or '(none)'}")
    print(f"skipped ({len(skipped)}): {', '.join(skipped) or '(none)'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
