/**
 * Languages the Musixmatch translation pipeline can target.
 * Mirrors `SUPPORTED_TRANSLATION_LANGS` in
 * `src-tauri/crates/app/src/commands/lyrics/settings.rs` exactly. Adding /
 * removing a code requires touching both the Rust whitelist (the
 * backend rejects unknown codes) and this list. The empty `value`
 * entry is the "off" sentinel — the dropdown's default state.
 */
export const TRANSLATION_LANG_OPTIONS: ReadonlyArray<{
  value: string;
  label: string;
}> = [
  { value: "", label: "Off" },
  { value: "en", label: "English" },
  { value: "es", label: "Español" },
  { value: "de", label: "Deutsch" },
  { value: "fr", label: "Français" },
  { value: "it", label: "Italiano" },
  { value: "nl", label: "Nederlands" },
  { value: "pt", label: "Português" },
  { value: "ru", label: "Русский" },
  { value: "tr", label: "Türkçe" },
  { value: "id", label: "Bahasa Indonesia" },
  { value: "ja", label: "日本語" },
  { value: "ko", label: "한국어" },
  { value: "zh-CN", label: "简体中文" },
  { value: "zh-TW", label: "繁體中文" },
  { value: "ar", label: "العربية" },
  { value: "hi", label: "हिन्दी" },
];
