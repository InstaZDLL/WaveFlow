import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import fr from "./locales/fr.json";
import en from "./locales/en.json";
import es from "./locales/es.json";
import de from "./locales/de.json";
import it from "./locales/it.json";
import zhTW from "./locales/zh-TW.json";
import zhCN from "./locales/zh-CN.json";
import pt from "./locales/pt.json";
import ptBR from "./locales/pt-BR.json";
import ja from "./locales/ja.json";
import kr from "./locales/kr.json";
import nl from "./locales/nl.json";
import ar from "./locales/ar.json";
import hi from "./locales/hi.json";
import ru from "./locales/ru.json";
import id from "./locales/id.json";
import tr from "./locales/tr.json";

export interface SupportedLanguage {
  code: string;
  nativeLabel: string;
}

// Ordered Latin-script first (Western then Eastern), then Cyrillic,
// then CJK, then Devanagari, then RTL. Mirrors the convention most
// music apps (Apple Music, Spotify) use so non-Latin scripts cluster
// at the bottom rather than being sprinkled through the list.
export const SUPPORTED_LANGUAGES: readonly SupportedLanguage[] = [
  // Latin — Western European
  { code: "en", nativeLabel: "English" },
  { code: "fr", nativeLabel: "Français" },
  { code: "de", nativeLabel: "Deutsch" },
  { code: "es", nativeLabel: "Español" },
  { code: "it", nativeLabel: "Italiano" },
  { code: "nl", nativeLabel: "Nederlands" },
  { code: "pt", nativeLabel: "Português" },
  { code: "pt-BR", nativeLabel: "Português (Brasil)" },
  // Latin — other
  { code: "tr", nativeLabel: "Türkçe" },
  { code: "id", nativeLabel: "Bahasa Indonesia" },
  // Cyrillic
  { code: "ru", nativeLabel: "Русский" },
  // CJK
  { code: "ja", nativeLabel: "日本語" },
  { code: "ko", nativeLabel: "한국어" },
  { code: "zh-CN", nativeLabel: "简体中文" },
  { code: "zh-TW", nativeLabel: "繁體中文" },
  // Devanagari
  { code: "hi", nativeLabel: "हिन्दी" },
  // RTL — last
  { code: "ar", nativeLabel: "العربية" },
] as const;

const LOCAL_STORAGE_KEY = "waveflow-language";
const SUPPORTED_LANGUAGE_CODES = SUPPORTED_LANGUAGES.map((lang) => lang.code);
// Map common BCP-47 regional variants we don't ship explicit resources
// for back to one of our supported codes. The browser language detector
// runs detected codes through `normalizeSupportedLanguageCode`, so an
// OS reporting `fr-FR` or `en-US` lands on `fr` / `en` instead of
// falling all the way to the fallback language.
const LANGUAGE_ALIASES: Record<string, string> = {
  // Korean — historical "kr" code (the file is still kr.json)
  kr: "ko",
  "ko-KR": "ko",
  // Chinese — pick simplified by default for ambiguous codes; preserve
  // explicit traditional/simplified BCP-47 distinctions.
  zh: "zh-CN",
  "zh-Hans": "zh-CN",
  "zh-Hans-CN": "zh-CN",
  "zh-SG": "zh-CN",
  "zh-Hant": "zh-TW",
  "zh-Hant-TW": "zh-TW",
  "zh-HK": "zh-TW",
  "zh-MO": "zh-TW",
  // Latin-script regional variants
  "fr-FR": "fr",
  "fr-CA": "fr",
  "fr-BE": "fr",
  "fr-CH": "fr",
  "en-US": "en",
  "en-GB": "en",
  "en-AU": "en",
  "en-CA": "en",
  "en-NZ": "en",
  "en-IE": "en",
  "es-ES": "es",
  "es-MX": "es",
  "es-AR": "es",
  "es-CL": "es",
  "es-CO": "es",
  "de-DE": "de",
  "de-AT": "de",
  "de-CH": "de",
  "it-IT": "it",
  "it-CH": "it",
  "pt-PT": "pt",
  "ja-JP": "ja",
  "nl-NL": "nl",
  "nl-BE": "nl",
  "ar-SA": "ar",
  "ar-EG": "ar",
  "ar-AE": "ar",
  "hi-IN": "hi",
  "ru-RU": "ru",
  "id-ID": "id",
  "tr-TR": "tr",
};

export function normalizeSupportedLanguageCode(code: string | undefined) {
  if (!code) return SUPPORTED_LANGUAGES[0].code;

  const normalized = LANGUAGE_ALIASES[code] ?? code;
  return SUPPORTED_LANGUAGE_CODES.includes(normalized) ? normalized : SUPPORTED_LANGUAGES[0].code;
}

function applyDocumentLanguage(code: string | undefined) {
  if (typeof document === "undefined") return;

  const normalizedCode = normalizeSupportedLanguageCode(code);
  document.documentElement.lang = normalizedCode;
  document.documentElement.dir = i18n.dir(normalizedCode);
}

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      fr: { translation: fr },
      en: { translation: en },
      es: { translation: es },
      de: { translation: de },
      it: { translation: it },
      "zh-TW": { translation: zhTW },
      "zh-CN": { translation: zhCN },
      pt: { translation: pt },
      "pt-BR": { translation: ptBR },
      ja: { translation: ja },
      ko: { translation: kr },
      kr: { translation: kr },
      nl: { translation: nl },
      ar: { translation: ar },
      hi: { translation: hi },
      ru: { translation: ru },
      id: { translation: id },
      tr: { translation: tr },
    },
    // English as fallback — universal enough that a user whose locale
    // we don't ship can still find their way around. (French is the
    // source language but is much narrower than English in practice.)
    fallbackLng: "en",
    supportedLngs: [...SUPPORTED_LANGUAGE_CODES, "kr"],
    // No `nonExplicitSupportedLngs`: it caused i18next to coerce
    // `zh-CN` and `zh-TW` to a bare `zh` code that has no resource,
    // silently falling back to `fallbackLng` even though the user
    // explicitly picked a Chinese variant. Regional variants are
    // pre-normalised via `convertDetectedLanguage` instead.
    interpolation: {
      escapeValue: false,
    },
    detection: {
      order: ["localStorage", "navigator"],
      caches: ["localStorage"],
      lookupLocalStorage: LOCAL_STORAGE_KEY,
      convertDetectedLanguage: normalizeSupportedLanguageCode,
    },
  })
  .then(() => {
    applyDocumentLanguage(i18n.resolvedLanguage ?? i18n.language);
  });

i18n.on("languageChanged", applyDocumentLanguage);

export default i18n;
