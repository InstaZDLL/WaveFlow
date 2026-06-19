import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { FileText, FileDown, Database } from "lucide-react";

import {
  getLyricsDefaultDestination,
  setLyricsDefaultDestination,
  type LyricsDestination,
} from "../../../lib/tauri/lyrics";

const OPTIONS: ReadonlyArray<{
  id: LyricsDestination;
  Icon: typeof FileText;
}> = [
  { id: "tag", Icon: FileText },
  { id: "sidecar", Icon: FileDown },
  { id: "db_only", Icon: Database },
];

/**
 * Settings → Playback row picking the editor's default save target
 * (issue #201).
 *
 * App-wide rather than per-profile because the destination drives
 * filesystem state (tag bytes, sidecar files) that is shared between
 * profiles whose libraries touch the same audio files — a per-profile
 * choice would create a false sense of isolation when two profiles
 * scan the same folder.
 */
export function LyricsDestinationCard() {
  const { t } = useTranslation();
  const [value, setValue] = useState<LyricsDestination>("tag");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getLyricsDefaultDestination().then(
      (v) => {
        if (cancelled) return;
        setValue(v);
      },
      (err) => {
        if (cancelled) return;
        console.warn(
          "[LyricsDestinationCard] read failed; keeping current default",
          err,
        );
      },
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const onPick = async (next: LyricsDestination) => {
    if (next === value || busy) return;
    setBusy(true);
    setError(null);
    const previous = value;
    setValue(next);
    try {
      await setLyricsDefaultDestination(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setValue(previous);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section
      aria-labelledby="settings-lyrics-destination-heading"
      className="px-4 py-3"
    >
      <header className="flex items-start gap-3 mb-3">
        <FileText
          size={20}
          className="text-zinc-400 mt-0.5 shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <h3
            id="settings-lyrics-destination-heading"
            className="text-sm font-medium text-zinc-900 dark:text-white"
          >
            {t("settings.lyricsDestination.title")}
          </h3>
          <p className="text-xs text-zinc-500 dark:text-zinc-400 leading-relaxed mt-0.5">
            {t("settings.lyricsDestination.subtitle")}
          </p>
        </div>
      </header>

      <div
        role="radiogroup"
        aria-labelledby="settings-lyrics-destination-heading"
        className="grid grid-cols-1 sm:grid-cols-3 gap-2"
      >
        {OPTIONS.map(({ id, Icon }) => {
          const selected = value === id;
          return (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={selected}
              disabled={busy}
              onClick={() => void onPick(id)}
              className={[
                "flex flex-col items-start gap-2 rounded-xl border p-3 text-left transition-all disabled:opacity-50",
                selected
                  ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-950/30 ring-1 ring-emerald-500/40"
                  : "border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 bg-white dark:bg-zinc-900",
              ].join(" ")}
            >
              <Icon
                size={18}
                className={
                  selected
                    ? "text-emerald-600 dark:text-emerald-400"
                    : "text-zinc-400"
                }
                aria-hidden="true"
              />
              <span className="text-sm font-medium text-zinc-900 dark:text-white">
                {t(`lyricsEditor.destination.${id}.label`)}
              </span>
              <span className="text-xs text-zinc-500 dark:text-zinc-400 leading-snug">
                {t(`lyricsEditor.destination.${id}.hint`)}
              </span>
            </button>
          );
        })}
      </div>

      {error && (
        <p
          role="alert"
          className="mt-2 text-xs text-red-600 dark:text-red-400"
        >
          {error}
        </p>
      )}
    </section>
  );
}
