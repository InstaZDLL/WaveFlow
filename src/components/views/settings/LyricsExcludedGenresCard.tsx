import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Tags, X } from "lucide-react";
import { useExcludedLyricsGenres } from "../../../hooks/useLyricsLookupSettings";

/**
 * Settings → Lyrics card listing the genres whose tracks skip the online
 * lyrics search (#721). Lyrics the file carries — its tag, a `.lrc` next
 * to it — are still read, and Refetch in the lyrics panel still searches
 * online: only the automatic lookup is skipped. Each entry also covers
 * its variants (`Lo-fi` covers `Lofi` and `lo-fi hip hop`).
 */
export function LyricsExcludedGenresCard() {
  const { t } = useTranslation();
  const { genres, ready, add, remove, reset } = useExcludedLyricsGenres();
  const [draft, setDraft] = useState("");

  const submit = () => {
    if (add(draft)) setDraft("");
  };

  return (
    <section
      aria-label={t("settings.lyricsExcludedGenres.title")}
      className="space-y-3 py-3"
    >
      <header className="px-4 flex items-start gap-3">
        <Tags
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3 className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.lyricsExcludedGenres.title")}
          </h3>
          <p className="mt-0.5 text-xs settings-description">
            {t("settings.lyricsExcludedGenres.subtitle")}
          </p>
        </div>
      </header>

      <div className="mx-4 space-y-3">
        {genres.length === 0 ? (
          <p className="text-xs settings-description">
            {t("settings.lyricsExcludedGenres.empty")}
          </p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {genres.map((genre) => (
              <li
                key={genre}
                className="flex items-center gap-1 pl-3 pr-1 py-1 rounded-full bg-zinc-100 dark:bg-zinc-800 text-sm text-zinc-800 dark:text-zinc-200"
              >
                <span>{genre}</span>
                <button
                  type="button"
                  disabled={!ready}
                  onClick={() => remove(genre)}
                  aria-label={t("settings.lyricsExcludedGenres.remove", {
                    genre,
                  })}
                  className="p-1 rounded-full text-zinc-500 hover:bg-zinc-200 hover:text-zinc-800 dark:hover:bg-zinc-700 dark:hover:text-white focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
                >
                  <X size={12} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        )}

        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <input
            type="text"
            value={draft}
            disabled={!ready}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={t("settings.lyricsExcludedGenres.placeholder")}
            aria-label={t("settings.lyricsExcludedGenres.placeholder")}
            className="flex-1 min-w-40 px-3 py-2 rounded-xl border border-zinc-200 bg-white text-sm text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
          />
          <button
            type="submit"
            disabled={!ready || !draft.trim()}
            className="px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {t("settings.lyricsExcludedGenres.add")}
          </button>
          <button
            type="button"
            disabled={!ready}
            onClick={reset}
            className="px-2 py-2 text-xs text-zinc-500 hover:text-zinc-800 dark:hover:text-white underline-offset-2 hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded disabled:opacity-50"
          >
            {t("settings.lyricsExcludedGenres.reset")}
          </button>
        </form>
      </div>
    </section>
  );
}
