use super::*;

/// Lyrics in the shape of an `.lrc`, in the language the encoding
/// is used for.
const SIMPLIFIED: &str = "[00:01.00]今天的天气很好，我们一起去公园散步吧\n\
     [00:05.50]阳光照在湖面上，风吹过树林的声音\n\
     [00:10.20]我们一边走一边唱着这首简单的歌";
const TRADITIONAL: &str = "[00:01.00]今天的天氣很好，我們一起去公園散步吧\n\
     [00:05.50]陽光照在湖面上，風吹過樹林的聲音\n\
     [00:10.20]我們一邊走一邊唱著這首簡單的歌";
const JAPANESE: &str = "[00:01.00]今日はいい天気ですね、一緒に公園を散歩しましょう\n\
     [00:05.50]湖に光が映って、風が森を通り抜ける音がする\n\
     [00:10.20]歩きながらこの簡単な歌を歌っています";
const KOREAN: &str = "[00:01.00]오늘은 날씨가 정말 좋네요, 함께 공원을 산책해요\n\
     [00:05.50]호수 위로 햇빛이 비치고 숲을 지나는 바람 소리\n\
     [00:10.20]우리는 걸으면서 이 간단한 노래를 불러요";
const WESTERN: &str = "[00:01.00]Café crème à la fenêtre, déjà l'été\n\
     [00:05.50]Où es-tu, ma belle, sous le ciel bleu de Noël";

fn round_trip(text: &str, encoding: &'static encoding_rs::Encoding) {
    let (bytes, _, unmappable) = encoding.encode(text);
    assert!(!unmappable, "{} cannot hold the sample", encoding.name());
    assert!(std::str::from_utf8(&bytes).is_err());
    assert_eq!(decode(&bytes), text, "{}", encoding.name());
}

#[test]
fn utf8_is_read_as_it_is_without_a_copy() {
    let decoded = decode(SIMPLIFIED.as_bytes());
    assert!(matches!(decoded, Cow::Borrowed(_)));
    assert_eq!(decoded, SIMPLIFIED);
}

#[test]
fn a_byte_order_mark_names_the_encoding_and_is_dropped() {
    let mut utf8 = vec![0xEF, 0xBB, 0xBF];
    utf8.extend_from_slice(SIMPLIFIED.as_bytes());
    assert_eq!(decode(&utf8), SIMPLIFIED);

    let mut utf16le = vec![0xFF, 0xFE];
    utf16le.extend(SIMPLIFIED.encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(decode(&utf16le), SIMPLIFIED);

    let mut utf16be = vec![0xFE, 0xFF];
    utf16be.extend(SIMPLIFIED.encode_utf16().flat_map(u16::to_be_bytes));
    assert_eq!(decode(&utf16be), SIMPLIFIED);
}

/// The case of #910: lyrics saved by a Chinese Windows tool.
#[test]
fn gb18030_lyrics_are_read() {
    round_trip(SIMPLIFIED, encoding_rs::GB18030);
}

/// What a fixed GB18030 fallback would have turned into nonsense.
#[test]
fn other_east_asian_encodings_are_told_apart() {
    round_trip(TRADITIONAL, encoding_rs::BIG5);
    round_trip(JAPANESE, encoding_rs::SHIFT_JIS);
    round_trip(KOREAN, encoding_rs::EUC_KR);
}

#[test]
fn an_old_western_file_is_read() {
    round_trip(WESTERN, encoding_rs::WINDOWS_1252);
}
