import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Play, Radio, Signal, Volume2 } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";
import {
  webRadioListStations,
  webRadioPlayStation,
  type WebRadioStation,
} from "../../lib/tauri/webRadio";

const ACCENTS = [
  "from-emerald-500 to-cyan-500",
  "from-indigo-500 to-sky-500",
  "from-rose-500 to-orange-500",
  "from-violet-500 to-fuchsia-500",
  "from-amber-500 to-lime-500",
];

export function WebRadioView() {
  const { t } = useTranslation();
  const { currentTrack, playbackState } = usePlayer();
  const [stations, setStations] = useState<WebRadioStation[]>([]);
  const [loading, setLoading] = useState(true);
  const [playingStationId, setPlayingStationId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    webRadioListStations()
      .then((rows) => {
        if (!cancelled) setStations(rows);
      })
      .catch((err) => {
        console.error("[WebRadioView] list stations failed", err);
        if (!cancelled) {
          setError(
            t("webRadio.errors.load", "Unable to load web radio stations."),
          );
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const activeStationSlug = useMemo(() => {
    if (!currentTrack || currentTrack.id >= -10_000) return null;
    return stations.find((station) => currentTrack.id === -10_000 - station.id)
      ?.slug;
  }, [currentTrack, stations]);

  const handlePlay = useCallback(
    async (station: WebRadioStation) => {
      if (playingStationId != null) return;
      setError(null);
      setPlayingStationId(station.id);
      try {
        await webRadioPlayStation(station.id);
      } catch (err) {
        console.error("[WebRadioView] play station failed", err);
        setError(
          t("webRadio.errors.play", "Unable to start {{station}}.", {
            station: station.name,
          }),
        );
      } finally {
        setPlayingStationId(null);
      }
    },
    [playingStationId, t],
  );

  return (
    <div className="space-y-8">
      <div className="flex flex-col gap-4 md:flex-row md:items-end md:justify-between">
        <div className="space-y-2">
          <div className="inline-flex items-center gap-2 text-xs font-semibold uppercase tracking-widest text-emerald-600 dark:text-emerald-400">
            <Signal size={14} />
            <span>{t("webRadio.eyebrow", "Live streams")}</span>
          </div>
          <div>
            <h1 className="text-3xl font-bold text-zinc-950 dark:text-white">
              {t("webRadio.title", "Web Radio")}
            </h1>
            <p className="mt-2 max-w-2xl text-sm leading-6 text-zinc-600 dark:text-zinc-400">
              {t(
                "webRadio.subtitle",
                "Tune into curated internet radio stations without adding anything to your local library.",
              )}
            </p>
          </div>
        </div>
        <div className="flex items-center gap-2 rounded-lg border border-zinc-200 bg-white px-3 py-2 text-xs font-medium text-zinc-600 shadow-sm dark:border-zinc-800 dark:bg-zinc-900 dark:text-zinc-300">
          <Radio size={15} />
          <span>
            {t("webRadio.stationCount", "{{count}} stations", {
              count: stations.length,
            })}
          </span>
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex min-h-72 items-center justify-center">
          <Loader2 size={28} className="animate-spin text-emerald-500" />
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {stations.map((station, index) => {
            const isActive =
              activeStationSlug === station.slug && playbackState !== "idle";
            const isStarting = playingStationId === station.id;
            return (
              <button
                key={station.slug}
                type="button"
                onClick={() => handlePlay(station)}
                disabled={isStarting}
                className={`group flex min-h-44 flex-col justify-between rounded-lg border p-4 text-left shadow-sm transition ${
                  isActive
                    ? "border-emerald-300 bg-emerald-50 dark:border-emerald-800 dark:bg-emerald-950/30"
                    : "border-zinc-200 bg-white hover:border-zinc-300 hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-900 dark:hover:border-zinc-700 dark:hover:bg-zinc-800"
                } disabled:opacity-70`}
              >
                <div className="flex items-start justify-between gap-4">
                  <div
                    className={`flex h-12 w-12 shrink-0 items-center justify-center rounded-lg bg-gradient-to-br ${ACCENTS[index % ACCENTS.length]} text-white shadow-sm`}
                  >
                    <Radio size={22} />
                  </div>
                  <div className="flex items-center gap-2 rounded-md bg-zinc-100 px-2 py-1 text-[11px] font-semibold uppercase text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300">
                    {station.codec}
                  </div>
                </div>

                <div className="mt-5 min-w-0">
                  <div className="flex items-center gap-2">
                    <h2 className="truncate text-lg font-semibold text-zinc-950 dark:text-white">
                      {station.name}
                    </h2>
                    {isActive && (
                      <Volume2
                        size={16}
                        className="shrink-0 text-emerald-600 dark:text-emerald-400"
                      />
                    )}
                  </div>
                  <p className="mt-1 line-clamp-2 text-sm leading-5 text-zinc-600 dark:text-zinc-400">
                    {station.tagline}
                  </p>
                </div>

                <div className="mt-5 flex items-center justify-between gap-3">
                  <span className="truncate text-xs font-medium text-zinc-500 dark:text-zinc-500">
                    {station.genre}
                  </span>
                  <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-zinc-950 text-white transition group-hover:bg-emerald-600 dark:bg-white dark:text-zinc-950 dark:group-hover:bg-emerald-400">
                    {isStarting ? (
                      <Loader2 size={16} className="animate-spin" />
                    ) : (
                      <Play size={16} className="ml-0.5 fill-current" />
                    )}
                  </span>
                </div>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
