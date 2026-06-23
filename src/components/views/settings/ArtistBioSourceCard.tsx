import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BookOpen, Globe } from "lucide-react";

import {
  getBioSource,
  setBioSource,
  getBioLanguage,
  setBioLanguage,
  BIO_LANGUAGES,
  type BioSource,
  type BioLanguage,
} from "../../../lib/tauri/integration";

const SOURCES: ReadonlyArray<{ id: BioSource; name: string; hintKey: string }> =
  [
    { id: "lastfm", name: "Last.fm", hintKey: "settings.artistBio.lastfmHint" },
    {
      id: "theaudiodb",
      name: "TheAudioDB",
      hintKey: "settings.artistBio.theaudiodbHint",
    },
  ];

/** Localized language name for a code, falling back to the upper-cased
 *  code when `Intl.DisplayNames` is unavailable / throws. */
function languageLabel(uiLang: string, code: string): string {
  try {
    const name = new Intl.DisplayNames([uiLang], { type: "language" }).of(code);
    if (name) return name.charAt(0).toUpperCase() + name.slice(1);
  } catch {
    /* fall through */
  }
  return code.toUpperCase();
}

/**
 * Settings → Integrations row choosing the artist-biography provider
 * (issue #295): Last.fm (English, needs the user's API key) or
 * TheAudioDB (multi-language community DB, no key). The language picker
 * only applies to TheAudioDB. App-wide, like the Last.fm key — bios
 * live in the shared metadata cache.
 */
export function ArtistBioSourceCard() {
  const { t, i18n } = useTranslation();
  const [source, setSource] = useState<BioSource>("lastfm");
  const [language, setLanguage] = useState<BioLanguage>("en");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const userTouchedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    Promise.all([getBioSource(), getBioLanguage()])
      .then(([s, l]) => {
        if (cancelled || userTouchedRef.current) return;
        setSource(s);
        setLanguage(l);
      })
      .catch((err) => {
        if (cancelled || userTouchedRef.current) return;
        console.warn("[ArtistBioSourceCard] read failed", err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const onPickSource = async (next: BioSource) => {
    if (next === source || busy) return;
    userTouchedRef.current = true;
    setBusy(true);
    setError(null);
    const previous = source;
    setSource(next);
    try {
      await setBioSource(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      // Re-sync from the source of truth rather than a possibly-stale
      // local default (the click may have raced initial hydration).
      getBioSource().then(setSource).catch(() => setSource(previous));
    } finally {
      setBusy(false);
    }
  };

  const onPickLanguage = async (next: BioLanguage) => {
    if (next === language || busy) return;
    userTouchedRef.current = true;
    setBusy(true);
    setError(null);
    const previous = language;
    setLanguage(next);
    try {
      await setBioLanguage(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      getBioLanguage().then(setLanguage).catch(() => setLanguage(previous));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      aria-labelledby="settings-artist-bio-heading"
      className="px-4 py-3"
    >
      <header className="flex items-start gap-3 mb-3">
        <BookOpen
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3
            id="settings-artist-bio-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.artistBio.title")}
          </h3>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.artistBio.subtitle")}
          </p>
        </div>
      </header>

      <div
        role="radiogroup"
        aria-labelledby="settings-artist-bio-heading"
        className="grid grid-cols-1 sm:grid-cols-2 gap-2"
      >
        {SOURCES.map(({ id, name, hintKey }) => {
          const selected = source === id;
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={busy}
              onClick={() => void onPickSource(id)}
              className={[
                "flex flex-col items-start gap-1 rounded-xl border p-3 text-left transition-all disabled:opacity-50",
                selected
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 ring-1 ring-emerald-500/40"
                  : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-900",
              ].join(" ")}
            >
              <span className="text-sm font-medium text-zinc-900 dark:text-white">
                {name}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400 leading-snug">
                {t(hintKey)}
              </span>
            </button>
          );
        })}
      </div>

      {source === "theaudiodb" && (
        <label className="mt-3 flex items-center justify-between gap-3">
          <span className="flex items-center gap-2 text-sm text-zinc-700 dark:text-zinc-300">
            <Globe size={16} className="text-zinc-400 shrink-0" aria-hidden="true" />
            {t("settings.artistBio.languageLabel")}
          </span>
          <select
            value={language}
            disabled={busy}
            onChange={(e) => void onPickLanguage(e.target.value as BioLanguage)}
            className="rounded-lg border border-zinc-200 bg-white px-3 py-1.5 text-sm text-zinc-800 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
          >
            {BIO_LANGUAGES.map((code) => (
              <option key={code} value={code}>
                {languageLabel(i18n.language, code)}
              </option>
            ))}
          </select>
        </label>
      )}

      {error && (
        <p role="alert" className="mt-2 text-xs text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
    </section>
  );
}
