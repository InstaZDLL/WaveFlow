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

export const SUPPORTED_LANGUAGES: readonly SupportedLanguage[] = [
  { code: "fr", nativeLabel: "Français" },
  { code: "en", nativeLabel: "English" },
  { code: "es", nativeLabel: "Español" },
  { code: "de", nativeLabel: "Deutsch" },
  { code: "it", nativeLabel: "Italiano" },
  { code: "zh-TW", nativeLabel: "繁體中文" },
  { code: "zh-CN", nativeLabel: "简体中文" },
  { code: "pt", nativeLabel: "Português" },
  { code: "pt-BR", nativeLabel: "Português (Brasil)" },
  { code: "ja", nativeLabel: "日本語" },
  { code: "ko", nativeLabel: "한국어" },
  { code: "nl", nativeLabel: "Nederlands" },
  { code: "ar", nativeLabel: "العربية" },
  { code: "hi", nativeLabel: "हिन्दी" },
  { code: "ru", nativeLabel: "Русский" },
  { code: "id", nativeLabel: "Bahasa Indonesia" },
  { code: "tr", nativeLabel: "Türkçe" },
] as const;

const LOCAL_STORAGE_KEY = "waveflow-language";
const SUPPORTED_LANGUAGE_CODES = SUPPORTED_LANGUAGES.map((lang) => lang.code);
const LANGUAGE_ALIASES: Record<string, string> = {
  kr: "ko",
  "ko-KR": "ko",
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
    fallbackLng: "fr",
    supportedLngs: [...SUPPORTED_LANGUAGE_CODES, "kr"],
    nonExplicitSupportedLngs: true,
    interpolation: {
      escapeValue: false,
    },
    detection: {
      order: ["localStorage", "navigator"],
      caches: ["localStorage"],
      lookupLocalStorage: LOCAL_STORAGE_KEY,
    },
  })
  .then(() => {
    applyDocumentLanguage(i18n.resolvedLanguage ?? i18n.language);
  });

i18n.on("languageChanged", applyDocumentLanguage);

export default i18n;
