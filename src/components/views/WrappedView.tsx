import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ChevronLeft,
  ChevronRight,
  Sparkles,
  X,
  Pause,
  Play,
  Loader2,
  Share2,
  Download,
  Copy,
  Check,
} from "lucide-react";
import {
  getWrapped,
  availableWrappedYears,
  wrappedCurrentYear,
  type WrappedPayload,
} from "../../lib/tauri/wrapped";
import type { ViewId } from "../../types";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { formatDuration } from "../../lib/tauri/track";
import { pickSaveFile } from "../../lib/tauri/dialog";
import { saveShareImage } from "../../lib/tauri/share";
import { renderWrappedCard } from "../../lib/wrappedCard";

interface WrappedViewProps {
  onNavigate: (view: ViewId) => void;
  initialYear: number | null;
  onNavigateToAlbum: (id: number) => void;
  onNavigateToArtist: (id: number) => void;
}

const SLIDE_MS = 6500;

type SlideId =
  | "intro"
  | "minutes"
  | "topTracks"
  | "topArtists"
  | "topAlbums"
  | "activeDay"
  | "mood"
  | "clock"
  | "streak"
  | "firstListen"
  | "months"
  | "outro";

export function WrappedView({
  onNavigate,
  initialYear,
  onNavigateToAlbum,
  onNavigateToArtist,
}: WrappedViewProps) {
  const { t, i18n } = useTranslation();

  const [year, setYear] = useState<number | null>(initialYear);
  const [availableYears, setAvailableYears] = useState<number[]>([]);
  const [payload, setPayload] = useState<WrappedPayload | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [slideIdx, setSlideIdx] = useState(0);
  const [paused, setPaused] = useState(false);
  const [shareOpen, setShareOpen] = useState(false);
  const [sharing, setSharing] = useState<
    "idle" | "saving" | "copying" | "done"
  >("idle");

  // ---- Year resolution + payload fetch ----
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [years, current] = await Promise.all([
          availableWrappedYears(),
          wrappedCurrentYear(),
        ]);
        if (cancelled) return;
        setAvailableYears(years);
        // If the parent didn't pre-select a year (or selected one with
        // no plays), fall back to the most recent year with data, then
        // to "current year" as a last resort so the view never empties.
        if (year == null) {
          setYear(years[0] ?? current);
        } else if (years.length > 0 && !years.includes(year)) {
          setYear(years[0]);
        }
      } catch (err) {
        console.error("[WrappedView] years load", err);
      }
    })();
    return () => {
      cancelled = true;
    };
    // year is intentionally not a dep here — we only want this on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (year == null) return;
    let cancelled = false;
    // Defer the loading flip + fetch to a microtask so React doesn't
    // flag this as a synchronous setState-in-effect cascade. The
    // user-visible behaviour is identical (one paint, then loader).
    queueMicrotask(() => {
      if (cancelled) return;
      setLoading(true);
      setError(null);
      setSlideIdx(0);
      getWrapped(year)
        .then((p) => {
          if (cancelled) return;
          setPayload(p);
        })
        .catch((err) => {
          if (cancelled) return;
          console.error("[WrappedView] load failed", err);
          setError(String(err));
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    });
    return () => {
      cancelled = true;
    };
  }, [year]);

  // ---- Slides — only those that have data ----
  const slides = useMemo<SlideId[]>(() => {
    if (!payload) return [];
    const list: SlideId[] = ["intro", "minutes"];
    if (payload.top_tracks.length > 0) list.push("topTracks");
    if (payload.top_artists.length > 0) list.push("topArtists");
    if (payload.top_albums.length > 0) list.push("topAlbums");
    if (payload.most_active_day) list.push("activeDay");
    if (payload.mood.avg_bpm != null) list.push("mood");
    list.push("clock");
    if (payload.streak && payload.streak.days >= 2) list.push("streak");
    if (payload.first_listen) list.push("firstListen");
    list.push("months", "outro");
    return list;
  }, [payload]);

  // ---- Auto-advance ----
  const tickRef = useRef<number | null>(null);
  useEffect(() => {
    if (paused || loading || error || slides.length === 0) return;
    tickRef.current = window.setTimeout(() => {
      setSlideIdx((i) => (i + 1 < slides.length ? i + 1 : i));
    }, SLIDE_MS);
    return () => {
      if (tickRef.current != null) {
        window.clearTimeout(tickRef.current);
        tickRef.current = null;
      }
    };
  }, [paused, loading, error, slides.length, slideIdx]);

  // ---- Keyboard ----
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onNavigate("home");
      } else if (e.key === "ArrowRight") {
        setSlideIdx((i) => (i + 1 < slides.length ? i + 1 : i));
      } else if (e.key === "ArrowLeft") {
        setSlideIdx((i) => (i > 0 ? i - 1 : 0));
      } else if (e.key === " ") {
        e.preventDefault();
        setPaused((p) => !p);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onNavigate, slides.length]);

  const goPrev = () => setSlideIdx((i) => (i > 0 ? i - 1 : 0));
  const goNext = () => setSlideIdx((i) => (i + 1 < slides.length ? i + 1 : i));

  // Share handlers — build the PNG once and dispatch by target.
  // Reasons each path lives here and not in a hook:
  //   - they depend on `payload` + i18n labels + the year accent, which
  //     all already live in this component's scope;
  //   - the "sharing" state is purely UI-local — no other view needs to
  //     observe whether the user just exported their card.
  const shareLabels = useMemo(
    () => ({
      wrapped: t("wrapped.share.brand"),
      yourYear: t("wrapped.intro.subtitle"),
      minutes: t("wrapped.minutes.eyebrow"),
      plays: t("wrapped.stats.plays"),
      artists: t("wrapped.stats.artists"),
      topTracks: t("wrapped.topTracks.title"),
      topArtists: t("wrapped.topArtists.title"),
      mood: t("wrapped.mood.eyebrow"),
      streak: t("wrapped.streak.eyebrow"),
      daysInARow: t("wrapped.streak.daysShort"),
      poweredBy: t("wrapped.share.poweredBy"),
    }),
    [t],
  );

  const buildCard = async (): Promise<Blob> => {
    if (!payload) throw new Error("payload missing");
    const accentNow = accentForSlide(payload.year, "intro");
    return renderWrappedCard(payload, accentNow, {
      labels: shareLabels,
      locale: i18n.language,
    });
  };

  const handleSaveImage = async () => {
    if (!payload) return;
    try {
      setSharing("saving");
      const defaultName = `WaveFlow-Wrapped-${payload.year}.png`;
      const target = await pickSaveFile(defaultName, ["png"]);
      if (!target) {
        setSharing("idle");
        return;
      }
      const blob = await buildCard();
      const bytes = new Uint8Array(await blob.arrayBuffer());
      await saveShareImage(bytes, target);
      setSharing("done");
      window.setTimeout(() => setSharing("idle"), 2000);
    } catch (err) {
      console.error("[WrappedView] save image failed", err);
      setSharing("idle");
    }
  };

  const handleCopyImage = async () => {
    if (!payload) return;
    try {
      setSharing("copying");
      const blob = await buildCard();
      // ClipboardItem is supported in Chromium-based WebView; Tauri 2
      // ships Edge WebView on Windows + WebKitGTK on Linux + WKWebView
      // on macOS — Linux's WebKitGTK is the only one that historically
      // refused image/png writes, so we surface the error rather than
      // silently no-op.
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blob }),
      ]);
      setSharing("done");
      window.setTimeout(() => setSharing("idle"), 2000);
    } catch (err) {
      console.error("[WrappedView] copy image failed", err);
      setSharing("idle");
    }
  };

  // Empty profile state — no plays this year. Pre-empts the loader
  // when years[] came back empty so we don't flash a spinner forever.
  if (!loading && payload && payload.total_plays === 0) {
    return <EmptyState onBack={() => onNavigate("home")} />;
  }
  if (!loading && availableYears.length === 0 && !payload) {
    return <EmptyState onBack={() => onNavigate("home")} />;
  }

  const current = slides[slideIdx];
  const accent = payload ? accentForSlide(payload.year, current) : null;

  return (
    <div className="fixed inset-0 z-100 bg-zinc-950 text-white overflow-hidden">
      {/* Full-screen accent backdrop — keyed on the current slide so
          the gradient cross-fades when the slide changes. Mounted at
          the overlay root (not inside each Slide) so it actually
          covers the whole viewport. */}
      {accent && (
        <div
          key={`bg-${current}`}
          aria-hidden="true"
          className="absolute inset-0 wrapped-fade-in"
          style={{
            background: `radial-gradient(circle at 30% 20%, ${accent.glow}, transparent 60%), radial-gradient(circle at 80% 70%, ${accent.glow2}, transparent 60%), ${accent.base}`,
          }}
        />
      )}

      {/* Top bar — progress segments + close */}
      <div className="absolute top-0 left-0 right-0 z-10 px-6 pt-4 flex items-center gap-3">
        <div className="flex-1 flex gap-1">
          {slides.map((_, i) => (
            <div
              key={i}
              className="flex-1 h-1 rounded-full bg-white/15 overflow-hidden"
            >
              <div
                className={`h-full bg-white ${
                  i < slideIdx
                    ? "w-full"
                    : i === slideIdx
                      ? paused
                        ? "w-0"
                        : "wrapped-segment"
                      : "w-0"
                }`}
                style={
                  i === slideIdx && !paused
                    ? { animationDuration: `${SLIDE_MS}ms` }
                    : undefined
                }
              />
            </div>
          ))}
        </div>
        <button
          onClick={() => setPaused((p) => !p)}
          className="p-2 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
          aria-label={paused ? t("wrapped.resume") : t("wrapped.pause")}
        >
          {paused ? <Play size={16} /> : <Pause size={16} />}
        </button>
        {availableYears.length > 1 && (
          <select
            value={year ?? ""}
            onChange={(e) => setYear(Number(e.target.value))}
            className="bg-white/10 hover:bg-white/20 transition-colors text-sm rounded-full px-3 py-1.5 focus:outline-none focus-visible:ring-2 focus-visible:ring-white/60"
            aria-label={t("wrapped.yearPicker")}
          >
            {availableYears.map((y) => (
              <option key={y} value={y} className="text-zinc-900">
                {y}
              </option>
            ))}
          </select>
        )}
        {payload && (
          <div className="relative">
            <button
              onClick={() => setShareOpen((s) => !s)}
              className="p-2 rounded-full bg-white/10 hover:bg-white/20 transition-colors disabled:opacity-50"
              disabled={sharing === "saving" || sharing === "copying"}
              aria-label={t("wrapped.share.open")}
            >
              {sharing === "saving" || sharing === "copying" ? (
                <Loader2 size={16} className="animate-spin" />
              ) : sharing === "done" ? (
                <Check size={16} />
              ) : (
                <Share2 size={16} />
              )}
            </button>
            {shareOpen && (
              <div className="absolute right-0 top-full mt-2 min-w-56 rounded-2xl bg-zinc-900/95 backdrop-blur-md border border-white/10 shadow-2xl overflow-hidden">
                <button
                  onClick={async () => {
                    setShareOpen(false);
                    await handleSaveImage();
                  }}
                  className="w-full px-4 py-3 flex items-center gap-3 hover:bg-white/10 transition-colors text-sm"
                >
                  <Download size={16} className="opacity-70" />
                  {t("wrapped.share.save")}
                </button>
                <button
                  onClick={async () => {
                    setShareOpen(false);
                    await handleCopyImage();
                  }}
                  className="w-full px-4 py-3 flex items-center gap-3 hover:bg-white/10 transition-colors text-sm border-t border-white/5"
                >
                  <Copy size={16} className="opacity-70" />
                  {t("wrapped.share.copy")}
                </button>
              </div>
            )}
          </div>
        )}
        <button
          onClick={() => onNavigate("home")}
          className="p-2 rounded-full bg-white/10 hover:bg-white/20 transition-colors"
          aria-label={t("wrapped.close")}
        >
          <X size={16} />
        </button>
      </div>

      {/* Tap zones — left/right thirds for prev/next */}
      <button
        type="button"
        onClick={goPrev}
        className="absolute top-16 bottom-0 left-0 w-1/3 z-5 group cursor-default"
        aria-label={t("wrapped.prev")}
      >
        <span className="absolute left-4 top-1/2 -translate-y-1/2 p-3 rounded-full bg-white/10 opacity-0 group-hover:opacity-100 transition-opacity">
          <ChevronLeft size={20} />
        </span>
      </button>
      <button
        type="button"
        onClick={goNext}
        className="absolute top-16 bottom-0 right-0 w-1/3 z-5 group cursor-default"
        aria-label={t("wrapped.next")}
      >
        <span className="absolute right-4 top-1/2 -translate-y-1/2 p-3 rounded-full bg-white/10 opacity-0 group-hover:opacity-100 transition-opacity">
          <ChevronRight size={20} />
        </span>
      </button>

      {/* Slide content */}
      <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
        <div className="w-full max-w-2xl mx-auto px-12 pointer-events-auto">
          {loading && (
            <div className="flex flex-col items-center text-white/80 gap-3">
              <Loader2 size={32} className="animate-spin" />
              <div className="text-sm">{t("wrapped.loading")}</div>
            </div>
          )}
          {error && (
            <div className="text-center space-y-4">
              <div className="text-xl font-semibold">
                {t("wrapped.errorTitle")}
              </div>
              <div className="text-sm text-white/70">{error}</div>
              <button
                onClick={() => onNavigate("home")}
                className="px-4 py-2 rounded-full bg-white text-zinc-900 font-semibold"
              >
                {t("wrapped.backToHome")}
              </button>
            </div>
          )}
          {payload && !loading && !error && (
            <SlideRenderer
              slide={current}
              payload={payload}
              onNavigateToAlbum={onNavigateToAlbum}
              onNavigateToArtist={onNavigateToArtist}
              onClose={() => onNavigate("home")}
            />
          )}
        </div>
      </div>
    </div>
  );
}

// =============================================================================
// Slide renderer
// =============================================================================

function SlideRenderer({
  slide,
  payload,
  onNavigateToAlbum,
  onNavigateToArtist,
  onClose,
}: {
  slide: SlideId | undefined;
  payload: WrappedPayload;
  onNavigateToAlbum: (id: number) => void;
  onNavigateToArtist: (id: number) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();

  switch (slide) {
    case "intro":
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <Sparkles className="mx-auto mb-6" size={48} />
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.intro.eyebrow")}
            </div>
            <h1 className="text-7xl font-extrabold leading-none mb-4">
              {payload.year}
            </h1>
            <p className="text-xl text-white/70">
              {t("wrapped.intro.subtitle")}
            </p>
          </div>
        </Slide>
      );

    case "minutes": {
      const minutes = Math.round(payload.total_listened_ms / 60000);
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.minutes.eyebrow")}
            </div>
            <div className="text-8xl font-extrabold leading-none mb-3 tabular-nums">
              {minutes.toLocaleString(i18n.language)}
            </div>
            <p className="text-xl text-white/70">
              {t("wrapped.minutes.subtitle", { count: minutes })}
            </p>
            <div className="mt-10 grid grid-cols-3 gap-4 text-center">
              <Stat
                value={payload.total_plays.toLocaleString(i18n.language)}
                label={t("wrapped.stats.plays")}
              />
              <Stat
                value={payload.unique_tracks.toLocaleString(i18n.language)}
                label={t("wrapped.stats.tracks")}
              />
              <Stat
                value={payload.unique_artists.toLocaleString(i18n.language)}
                label={t("wrapped.stats.artists")}
              />
            </div>
          </div>
        </Slide>
      );
    }

    case "topTracks":
      return (
        <Slide>
          <div className="wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-6 text-center">
              {t("wrapped.topTracks.title")}
            </div>
            <ol className="space-y-3">
              {payload.top_tracks.slice(0, 5).map((tr, idx) => {
                const src = resolveRemoteImage(
                  tr.artwork_path_2x ?? tr.artwork_path,
                  null,
                );
                return (
                  <li
                    key={tr.track_id}
                    className="flex items-center gap-4 bg-white/10 rounded-2xl p-3 backdrop-blur-sm"
                  >
                    <div className="text-3xl font-extrabold tabular-nums w-10 text-center text-white/70">
                      {idx + 1}
                    </div>
                    <div className="w-14 h-14 rounded-lg bg-white/10 overflow-hidden flex-shrink-0">
                      {src && (
                        <img
                          src={src}
                          alt=""
                          className="w-full h-full object-cover"
                        />
                      )}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="font-semibold truncate">{tr.title}</div>
                      <div className="text-sm text-white/60 truncate">
                        {tr.artist_name ?? "—"}
                      </div>
                    </div>
                    <div className="text-sm text-white/60 tabular-nums">
                      {t("wrapped.playsShort", { count: tr.plays })}
                    </div>
                  </li>
                );
              })}
            </ol>
          </div>
        </Slide>
      );

    case "topArtists":
      return (
        <Slide>
          <div className="wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-6 text-center">
              {t("wrapped.topArtists.title")}
            </div>
            <ol className="space-y-3">
              {payload.top_artists.slice(0, 5).map((ar, idx) => {
                const src = resolveRemoteImage(
                  ar.picture_path_2x ?? ar.picture_path,
                  ar.picture_url,
                );
                return (
                  <li
                    key={ar.artist_id}
                    className="flex items-center gap-4 bg-white/10 rounded-2xl p-3 backdrop-blur-sm cursor-pointer hover:bg-white/15"
                    onClick={() => onNavigateToArtist(ar.artist_id)}
                  >
                    <div className="text-3xl font-extrabold tabular-nums w-10 text-center text-white/70">
                      {idx + 1}
                    </div>
                    <div className="w-14 h-14 rounded-full bg-white/10 overflow-hidden flex-shrink-0">
                      {src && (
                        <img
                          src={src}
                          alt=""
                          className="w-full h-full object-cover"
                        />
                      )}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="font-semibold truncate">{ar.name}</div>
                      <div className="text-sm text-white/60 truncate">
                        {t("wrapped.playsShort", { count: ar.plays })}
                      </div>
                    </div>
                  </li>
                );
              })}
            </ol>
          </div>
        </Slide>
      );

    case "topAlbums":
      return (
        <Slide>
          <div className="wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-6 text-center">
              {t("wrapped.topAlbums.title")}
            </div>
            <div className="grid grid-cols-3 gap-4">
              {payload.top_albums.slice(0, 3).map((al) => {
                const src = resolveRemoteImage(
                  al.artwork_path_2x ?? al.artwork_path,
                  null,
                );
                return (
                  <button
                    key={al.album_id}
                    onClick={() => onNavigateToAlbum(al.album_id)}
                    className="bg-white/10 rounded-2xl p-3 hover:bg-white/15 transition-colors backdrop-blur-sm text-left"
                  >
                    <div className="aspect-square rounded-xl bg-white/10 overflow-hidden mb-3">
                      {src && (
                        <img
                          src={src}
                          alt=""
                          className="w-full h-full object-cover"
                        />
                      )}
                    </div>
                    <div className="font-semibold truncate text-sm">
                      {al.title}
                    </div>
                    <div className="text-xs text-white/60 truncate">
                      {al.artist_name ?? "—"}
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
        </Slide>
      );

    case "activeDay": {
      const day = payload.most_active_day!;
      const formatted = formatDayLong(day.day, i18n.language);
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.activeDay.eyebrow")}
            </div>
            <div className="text-5xl font-extrabold mb-4">{formatted}</div>
            <p className="text-xl text-white/70">
              {t("wrapped.activeDay.subtitle", {
                duration: formatDuration(day.listened_ms),
                count: day.plays,
              })}
            </p>
          </div>
        </Slide>
      );
    }

    case "mood": {
      const bpm = payload.mood.avg_bpm ?? 0;
      const energy = payload.mood.energy ?? "warm";
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.mood.eyebrow")}
            </div>
            <div className="text-7xl font-extrabold leading-none mb-2 tabular-nums">
              {Math.round(bpm)}
              <span className="text-2xl ml-2 text-white/60">BPM</span>
            </div>
            <p className="text-xl text-white/70 mb-6">
              {t("wrapped.mood.subtitle")}
            </p>
            <div className="inline-block text-xl font-bold uppercase tracking-widest px-6 py-2 rounded-full bg-white/15 backdrop-blur-sm">
              {t(`wrapped.mood.energy.${energy}`)}
            </div>
            {payload.mood.avg_lufs != null && (
              <div className="mt-6 text-sm text-white/60 tabular-nums">
                {t("wrapped.mood.loudness", {
                  lufs: payload.mood.avg_lufs.toFixed(1),
                })}
              </div>
            )}
          </div>
        </Slide>
      );
    }

    case "clock": {
      const max = Math.max(1, ...payload.by_hour);
      const peakHour = payload.by_hour.indexOf(Math.max(...payload.by_hour));
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.clock.eyebrow")}
            </div>
            <div className="text-5xl font-extrabold mb-2 tabular-nums">
              {peakHour.toString().padStart(2, "0")}:00
            </div>
            <p className="text-xl text-white/70 mb-8">
              {t("wrapped.clock.subtitle")}
            </p>
            <div className="flex items-end justify-center gap-1 h-32">
              {payload.by_hour.map((v, h) => (
                <div
                  key={h}
                  className="flex-1 max-w-[18px] bg-white/70 rounded-t"
                  style={{
                    height: `${(v / max) * 100}%`,
                    minHeight: v > 0 ? "4px" : "1px",
                    opacity: v > 0 ? 1 : 0.2,
                  }}
                  title={`${h.toString().padStart(2, "0")}:00 — ${v}`}
                />
              ))}
            </div>
          </div>
        </Slide>
      );
    }

    case "streak": {
      const s = payload.streak!;
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.streak.eyebrow")}
            </div>
            <div className="text-8xl font-extrabold leading-none mb-2 tabular-nums">
              {s.days}
            </div>
            <p className="text-2xl text-white/80 mb-2">
              {t("wrapped.streak.daysLabel", { count: s.days })}
            </p>
            <p className="text-sm text-white/60">
              {t("wrapped.streak.range", {
                start: formatDayShort(s.start, i18n.language),
                end: formatDayShort(s.end, i18n.language),
              })}
            </p>
          </div>
        </Slide>
      );
    }

    case "firstListen": {
      const f = payload.first_listen!;
      const date = new Date(f.played_at);
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-3">
              {t("wrapped.firstListen.eyebrow")}
            </div>
            <div className="text-4xl font-extrabold mb-2">{f.title}</div>
            <div className="text-lg text-white/70 mb-6">
              {f.artist_name ?? "—"}
            </div>
            <div className="text-sm text-white/50">
              {date.toLocaleString(i18n.language, {
                weekday: "long",
                day: "numeric",
                month: "long",
                hour: "2-digit",
                minute: "2-digit",
              })}
            </div>
          </div>
        </Slide>
      );
    }

    case "months": {
      const max = Math.max(1, ...payload.by_month.map((m) => m.listened_ms));
      const monthLabels = monthsShort(i18n.language);
      return (
        <Slide>
          <div className="wrapped-fade-up">
            <div className="uppercase tracking-[0.4em] text-xs text-white/60 mb-6 text-center">
              {t("wrapped.months.title")}
            </div>
            <div className="flex items-end gap-2 h-48">
              {payload.by_month.map((m, idx) => (
                <div key={idx} className="flex-1 flex flex-col items-center">
                  <div
                    className="w-full bg-white/70 rounded-t"
                    style={{
                      height: `${(m.listened_ms / max) * 100}%`,
                      minHeight: m.listened_ms > 0 ? "6px" : "2px",
                      opacity: m.listened_ms > 0 ? 1 : 0.2,
                    }}
                    title={`${monthLabels[idx]}: ${formatDuration(m.listened_ms)}`}
                  />
                  <div className="text-[10px] text-white/60 mt-1 uppercase tabular-nums">
                    {monthLabels[idx]}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </Slide>
      );
    }

    case "outro":
      return (
        <Slide>
          <div className="text-center wrapped-fade-up">
            <Sparkles className="mx-auto mb-6" size={48} />
            <h2 className="text-4xl font-extrabold mb-3">
              {t("wrapped.outro.title")}
            </h2>
            <p className="text-lg text-white/70 mb-8">
              {t("wrapped.outro.subtitle")}
            </p>
            <button
              onClick={onClose}
              className="px-6 py-3 rounded-full bg-white text-zinc-900 font-semibold hover:scale-105 transition-transform"
            >
              {t("wrapped.outro.cta")}
            </button>
          </div>
        </Slide>
      );

    default:
      return null;
  }
}

function Slide({ children }: { children: React.ReactNode }) {
  return <div className="relative w-full">{children}</div>;
}

function Stat({ value, label }: { value: string; label: string }) {
  return (
    <div>
      <div className="text-2xl font-bold tabular-nums">{value}</div>
      <div className="text-xs uppercase tracking-wider text-white/60 mt-1">
        {label}
      </div>
    </div>
  );
}

function EmptyState({ onBack }: { onBack: () => void }) {
  const { t } = useTranslation();
  return (
    <div className="fixed inset-0 z-100 bg-zinc-950 text-white flex items-center justify-center px-12">
      <div className="text-center max-w-md">
        <Sparkles className="mx-auto mb-6 opacity-50" size={48} />
        <h2 className="text-3xl font-bold mb-2">{t("wrapped.empty.title")}</h2>
        <p className="text-white/70 mb-8">{t("wrapped.empty.subtitle")}</p>
        <button
          onClick={onBack}
          className="px-6 py-3 rounded-full bg-white text-zinc-900 font-semibold"
        >
          {t("wrapped.backToHome")}
        </button>
      </div>
    </div>
  );
}

// =============================================================================
// Helpers
// =============================================================================

type Accent = { base: string; glow: string; glow2: string };

const ACCENT_PALETTES: Accent[] = [
  {
    base: "linear-gradient(135deg,#1d0e3a 0%,#3a1052 100%)",
    glow: "rgba(217,70,239,0.55)",
    glow2: "rgba(99,102,241,0.45)",
  },
  {
    base: "linear-gradient(135deg,#0b1a3a 0%,#1e3a8a 100%)",
    glow: "rgba(56,189,248,0.55)",
    glow2: "rgba(168,85,247,0.45)",
  },
  {
    base: "linear-gradient(135deg,#2a0a18 0%,#7c2d12 100%)",
    glow: "rgba(251,146,60,0.6)",
    glow2: "rgba(244,63,94,0.45)",
  },
  {
    base: "linear-gradient(135deg,#0a2a1f 0%,#065f46 100%)",
    glow: "rgba(52,211,153,0.55)",
    glow2: "rgba(20,184,166,0.45)",
  },
  {
    base: "linear-gradient(135deg,#1a0820 0%,#5b21b6 100%)",
    glow: "rgba(192,132,252,0.55)",
    glow2: "rgba(236,72,153,0.45)",
  },
];

const SLIDE_ORDER: SlideId[] = [
  "intro",
  "minutes",
  "topTracks",
  "topArtists",
  "topAlbums",
  "activeDay",
  "mood",
  "clock",
  "streak",
  "firstListen",
  "months",
  "outro",
];

/**
 * Pick a palette per (year, slide). The base offset comes from the
 * year so every "2026 Wrapped" feels like a season, and each slide
 * rotates one step further down the palette list so the overlay
 * doesn't feel monochromatic across 10+ slides.
 */
function accentForSlide(year: number, slide: SlideId | undefined): Accent {
  const yearOffset =
    ((year % ACCENT_PALETTES.length) + ACCENT_PALETTES.length) %
    ACCENT_PALETTES.length;
  const slideOffset = slide ? Math.max(0, SLIDE_ORDER.indexOf(slide)) : 0;
  return ACCENT_PALETTES[(yearOffset + slideOffset) % ACCENT_PALETTES.length];
}

function formatDayLong(day: string, locale: string): string {
  const d = new Date(`${day}T00:00:00`);
  return d.toLocaleDateString(locale, {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

function formatDayShort(day: string, locale: string): string {
  const d = new Date(`${day}T00:00:00`);
  return d.toLocaleDateString(locale, {
    day: "numeric",
    month: "short",
  });
}

function monthsShort(locale: string): string[] {
  // Build localised short month labels once per render. `Intl` handles
  // every supported locale including ar/hi/zh — no need to ship 12
  // strings × 17 locales in the i18n bundles.
  return Array.from({ length: 12 }, (_, i) => {
    const d = new Date(2026, i, 1);
    return d.toLocaleDateString(locale, { month: "short" });
  });
}
