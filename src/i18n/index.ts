import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import fr from "./locales/fr.json";
import en from "./locales/en.json";

export interface SupportedLanguage {
  code: string;
  nativeLabel: string;
}

export const SUPPORTED_LANGUAGES: readonly SupportedLanguage[] = [
  { code: "fr", nativeLabel: "Français" },
  { code: "en", nativeLabel: "English" },
] as const;

const LOCAL_STORAGE_KEY = "waveflow-language";

i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      fr: { translation: fr },
      en: { translation: en },
    },
    fallbackLng: "fr",
    supportedLngs: SUPPORTED_LANGUAGES.map((lang) => lang.code),
    nonExplicitSupportedLngs: true,
    interpolation: {
      escapeValue: false,
    },
    detection: {
      order: ["localStorage", "navigator"],
      caches: ["localStorage"],
      lookupLocalStorage: LOCAL_STORAGE_KEY,
    },
  });

export default i18n;
