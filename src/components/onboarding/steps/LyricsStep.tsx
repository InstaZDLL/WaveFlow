import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Database, FileDown, FileText } from "lucide-react";
import {
  getLyricsDefaultDestination,
  setLyricsDefaultDestination,
  type LyricsDestination,
} from "../../../lib/tauri/lyrics";
import { usageOf, usePluginCatalogue } from "../usePluginCatalogue";
import { PluginUsageCard } from "./PluginUsageCard";
import { StepHeading, StepSection } from "./StepHeading";

const DESTINATIONS = [
  { id: "tag", Icon: FileText },
  { id: "sidecar", Icon: FileDown },
  { id: "db_only", Icon: Database },
] as const;

/**
 * Lyrics: where the ones WaveFlow finds or the user writes are kept
 * (the same choice as Settings → Lyrics, written at once), and the
 * sources the store adds, word-synced ones first.
 */
export function LyricsStep({
  onTouch,
  plugins,
}: {
  onTouch: () => void;
  plugins: ReturnType<typeof usePluginCatalogue>;
}) {
  const { t } = useTranslation();
  const [destination, setDestination] = useState<LyricsDestination | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  // A pick made before the stored value arrived wins over that value.
  const touchedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    getLyricsDefaultDestination()
      .then((value) => {
        if (!cancelled && !touchedRef.current) setDestination(value);
      })
      .catch((err) =>
        console.error("[Onboarding] read lyrics destination failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, []);

  const pick = async (next: LyricsDestination) => {
    if (next === destination || busy) return;
    onTouch();
    touchedRef.current = true;
    const previous = destination;
    setBusy(true);
    setDestination(next);
    try {
      await setLyricsDefaultDestination(next);
    } catch (err) {
      console.error("[Onboarding] set lyrics destination failed", err);
      setDestination(previous);
    } finally {
      setBusy(false);
    }
  };

  const { catalogue, installs, install } = plugins;
  const available = catalogue.status === "ready";

  return (
    <div>
      <StepHeading
        title={t("onboarding.lyrics.title")}
        lede={t("onboarding.lyrics.lede")}
      />
      <StepSection label={t("onboarding.lyrics.destination")}>
        <div
          role="radiogroup"
          aria-label={t("onboarding.lyrics.destination")}
          className="grid grid-cols-1 gap-2 sm:grid-cols-3"
        >
          {DESTINATIONS.map(({ id, Icon }) => {
            const active = destination === id;
            return (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={active}
                disabled={busy || destination == null}
                onClick={() => void pick(id)}
                className={`onboarding-tile rounded-2xl border p-3 text-left transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-60 ${
                  active
                    ? "border-emerald-500 bg-emerald-50 ring-2 ring-emerald-500/30 dark:bg-emerald-500/10"
                    : "border-zinc-200 bg-white hover:border-zinc-300 dark:border-zinc-800 dark:bg-zinc-900 dark:hover:border-zinc-700"
                }`}
              >
                <div className="flex items-start justify-between gap-2">
                  <Icon
                    size={18}
                    aria-hidden="true"
                    className={active ? "text-emerald-500" : "text-zinc-400"}
                  />
                  {active && (
                    <Check
                      size={14}
                      strokeWidth={3}
                      className="text-emerald-500"
                    />
                  )}
                </div>
                <div className="mt-2 text-sm font-semibold text-zinc-900 dark:text-white">
                  {t(`lyricsEditor.destination.${id}.label`)}
                </div>
                <div className="mt-1 text-[11px] leading-snug text-zinc-500 dark:text-zinc-400">
                  {t(`lyricsEditor.destination.${id}.hint`)}
                </div>
              </button>
            );
          })}
        </div>
      </StepSection>
      <StepSection label={t("onboarding.lyrics.sources")}>
        {catalogue.status !== "loading" && (
          <PluginUsageCard
            usage="lyrics"
            available={available}
            entries={
              available
                ? catalogue.entries.filter((e) => usageOf(e) === "lyrics")
                : []
            }
            installs={installs}
            onInstall={(id) => {
              onTouch();
              void install(id);
            }}
          />
        )}
      </StepSection>
    </div>
  );
}
