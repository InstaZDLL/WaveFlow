import i18n from "i18next";
import { setTrayLabels } from "../lib/tauri/tray";
import type {
  BackendModule,
  InitOptions,
  ReadCallback,
  ResourceLanguage,
  Services,
} from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

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

// Rewrite the historical "kr" preference to "ko" once, before i18next
// reads localStorage. `convertDetectedLanguage` already normalises at
// read time, but persisting the canonical value keeps DevTools / future
// migrations honest.
try {
  if (typeof localStorage !== "undefined") {
    if (localStorage.getItem(LOCAL_STORAGE_KEY) === "kr") {
      localStorage.setItem(LOCAL_STORAGE_KEY, "ko");
    }
  }
} catch {
  // Storage unavailable (private browsing, embed sandbox) — non-fatal,
  // the in-memory alias still maps "kr" → "ko" for the session.
}
const localeLoaders: Record<
  string,
  () => Promise<{ default: ResourceLanguage }>
> = {
  fr: () => import("./locales/fr.json"),
  en: () => import("./locales/en.json"),
  es: () => import("./locales/es.json"),
  de: () => import("./locales/de.json"),
  it: () => import("./locales/it.json"),
  "zh-TW": () => import("./locales/zh-TW.json"),
  "zh-CN": () => import("./locales/zh-CN.json"),
  pt: () => import("./locales/pt.json"),
  "pt-BR": () => import("./locales/pt-BR.json"),
  ja: () => import("./locales/ja.json"),
  ko: () => import("./locales/ko.json"),
  nl: () => import("./locales/nl.json"),
  ar: () => import("./locales/ar.json"),
  hi: () => import("./locales/hi.json"),
  ru: () => import("./locales/ru.json"),
  id: () => import("./locales/id.json"),
  tr: () => import("./locales/tr.json"),
};
// Map common BCP-47 regional variants we don't ship explicit resources
// for back to one of our supported codes. The browser language detector
// runs detected codes through `normalizeSupportedLanguageCode`, so an
// OS reporting `fr-FR` or `en-US` lands on `fr` / `en` instead of
// falling all the way to the fallback language.
const LANGUAGE_ALIASES: Record<string, string> = {
  // Korean — historical "kr" code that shipped before we normalised to
  // the correct ISO 639-1 "ko". Old profiles may still have "kr" in
  // localStorage; the alias keeps them working until they pick a
  // language explicitly.
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
  return SUPPORTED_LANGUAGE_CODES.includes(normalized)
    ? normalized
    : SUPPORTED_LANGUAGES[0].code;
}

function applyDocumentLanguage(code: string | undefined) {
  if (typeof document === "undefined") return;

  const normalizedCode = normalizeSupportedLanguageCode(code);
  document.documentElement.lang = normalizedCode;
  document.documentElement.dir = i18n.dir(normalizedCode);
}

// Push the localised tray menu labels to the Rust backend. The tray
// is built at startup with English seed strings (frontend hasn't had
// time to load i18next yet); this re-titles each item once the user
// language is known and on every subsequent `languageChanged`.
function pushTrayLabels() {
  setTrayLabels({
    playPause: i18n.t("system.tray.playPause"),
    previous: i18n.t("system.tray.previous"),
    next: i18n.t("system.tray.next"),
    show: i18n.t("system.tray.show"),
    quit: i18n.t("system.tray.quit"),
  }).catch(() => {
    // Tauri command unavailable (e.g. running outside the desktop
    // shell during a Vite-only dev session) — drop silently.
  });
}

const dynamicLocaleBackend: BackendModule = {
  type: "backend",
  init(
    _services: Services,
    _backendOptions: object,
    _i18nextOptions: InitOptions,
  ) {},
  read(language: string, _namespace: string, callback: ReadCallback) {
    const code = normalizeSupportedLanguageCode(language);
    const loadLocale = localeLoaders[code] ?? localeLoaders.en;

    loadLocale()
      .then((module) => callback(null, module.default))
      .catch((err: unknown) => {
        console.error(`[i18n] failed to load locale "${code}"`, err);
        callback(err instanceof Error ? err : String(err), null);
      });
  },
};

export const i18nReady = i18n
  .use(dynamicLocaleBackend)
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
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
    react: {
      useSuspense: false,
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
    pushTrayLabels();
  });

i18n.on("languageChanged", (code) => {
  applyDocumentLanguage(code);
  pushTrayLabels();
});

export default i18n;
