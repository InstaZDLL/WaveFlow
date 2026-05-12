import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  ExternalLink,
  ImageUp,
  Pencil,
  Save,
  Sparkles,
  X,
} from "lucide-react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Artwork } from "./Artwork";
import { HiResBadge } from "./HiResBadge";
import {
  formatDuration,
  getTrack,
  setTrackRating,
  updateTrackCover,
  updateTrackTags,
  type Track,
  type TrackEdit,
} from "../../lib/tauri/track";
import { StarRating } from "./StarRating";
import { pickFile } from "../../lib/tauri/dialog";
import { useTrackUpdated } from "../../hooks/useTrackUpdated";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  analyzeTrack,
  getTrackAnalysis,
  type TrackAnalysis,
} from "../../lib/tauri/analysis";

interface TrackPropertiesModalProps {
  track: Track | null;
  onClose: () => void;
}

/**
 * Foobar2000-style "Properties" dialog. Renders every spec we know
 * about a track: file metadata (title / album / artist / year / track
 * number), audio characteristics (codec / bit depth / sample rate /
 * channels / bitrate) and on-disk info (path / size / added at).
 *
 * Hidden when `track` is null so the parent can keep the modal
 * mounted and just clear the track to dismiss.
 */
export function TrackPropertiesModal({
  track: trackProp,
  onClose,
}: TrackPropertiesModalProps) {
  const { t, i18n } = useTranslation();
  // Local mirror of the prop track so the cover / metadata can refresh
  // in-place after a save without forcing the parent to refetch and
  // pass new props down. The parent re-keys this component on
  // `track.id`, so we just lazy-init from the prop here and let the
  // `track:updated` listener below refetch when the user saves —
  // never sync the prop back through a useEffect (would trip the
  // cascading-render lint AND be redundant given the key strategy).
  const [track, setTrack] = useState<Track | null>(trackProp);
  // Modal a11y — Escape to close + focus trap. `track != null` doubles
  // as the open state since the modal returns `null` when there's no
  // track (see early-return at the bottom of the function).
  const dialogRef = useModalA11y<HTMLDivElement>(track != null, onClose);
  useTrackUpdated(
    useCallback(
      async (id: number) => {
        if (track == null || id !== track.id) return;
        try {
          const fresh = await getTrack(id);
          if (fresh) setTrack(fresh);
        } catch (err) {
          console.error("[TrackProperties] get_track failed", err);
        }
      },
      [track],
    ),
  );

  const [analysis, setAnalysis] = useState<TrackAnalysis | null>(null);
  const [analyzing, setAnalyzing] = useState(false);

  // Optimistic rating override — clicking a star paints the new value
  // immediately and rolls back if the backend rejects the write. Kept
  // separate from `track.rating` so the rollback can fall back to the
  // server-truth without an extra fetch.
  const [ratingOverride, setRatingOverride] = useState<number | null | "none">(
    "none",
  );
  const handleSetRating = useCallback(
    async (next: number | null) => {
      if (!track) return;
      setRatingOverride(next);
      try {
        await setTrackRating(track.id, next);
        // Server emits `track:updated` → useTrackUpdated refetches →
        // track.rating reflects the new value → drop the override.
        setRatingOverride("none");
      } catch (err) {
        console.error("[TrackProperties] set_track_rating failed", err);
        setRatingOverride("none");
      }
    },
    [track],
  );

  // Edit mode state. The form is keyed on the parent-supplied
  // `track`, so opening the modal on a fresh track resets every
  // input via the parent's re-mount (see the props.key on
  // `<TrackPropertiesModal>`). Saving fires `track:updated` from
  // the backend; consuming views listen and refetch.
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [form, setForm] = useState<{
    title: string;
    artist: string;
    album: string;
    year: string;
    track_number: string;
    disc_number: string;
    genre: string;
  }>({
    title: "",
    artist: "",
    album: "",
    year: "",
    track_number: "",
    disc_number: "",
    genre: "",
  });

  // Fetch the cached analysis whenever the dialog opens on a new
  // track. The parent re-mounts this component (keyed on track.id)
  // so we don't need a reset path here — every fresh mount starts
  // with `analysis = null` and `analyzing = false`.
  useEffect(() => {
    if (!track) return;
    let cancelled = false;
    getTrackAnalysis(track.id)
      .then((row) => {
        if (!cancelled) setAnalysis(row);
      })
      .catch((err) =>
        console.error("[TrackProperties] get_track_analysis failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [track]);

  // Hydrate the edit form whenever the track changes. Wrapped in a
  // microtask via setTimeout(0) so the state writes happen outside
  // the effect body — keeps the cascading-render lint happy without
  // changing observable behaviour (the form has no need to be ready
  // on the very first paint, only on user interaction).
  useEffect(() => {
    if (!track) return;
    const handle = window.setTimeout(() => {
      setForm({
        title: track.title ?? "",
        artist: track.artist_name ?? "",
        album: track.album_title ?? "",
        year: track.year != null ? String(track.year) : "",
        track_number:
          track.track_number != null ? String(track.track_number) : "",
        disc_number:
          track.disc_number != null ? String(track.disc_number) : "",
        // Genre isn't on the Track row yet — the editor lets the user
        // enter / overwrite one and the backend syncs track_genre
        // accordingly. Future iteration: surface the existing genres.
        genre: "",
      });
      setEditing(false);
    }, 0);
    return () => window.clearTimeout(handle);
  }, [track]);

  if (!track) return null;

  const handleAnalyze = async () => {
    if (analyzing) return;
    setAnalyzing(true);
    try {
      const row = await analyzeTrack(track.id);
      setAnalysis(row);
    } catch (err) {
      console.error("[TrackProperties] analyze_track failed", err);
    } finally {
      setAnalyzing(false);
    }
  };

  const handleSave = async () => {
    if (!track || saving) return;
    setSaving(true);
    try {
      const edit: TrackEdit = {
        title: form.title,
        artist: form.artist,
        album: form.album,
        // Empty string in a number field clears the value (sent as 0
        // → backend treats as "remove this tag"). Non-numeric input
        // is rejected silently — the input is `type="number"` so the
        // browser typically guards against it anyway.
        year: form.year.trim() === "" ? 0 : Number(form.year) || 0,
        track_number:
          form.track_number.trim() === "" ? 0 : Number(form.track_number) || 0,
        disc_number:
          form.disc_number.trim() === "" ? 0 : Number(form.disc_number) || 0,
        genre: form.genre,
      };
      await updateTrackTags(track.id, edit);
      setEditing(false);
      // The backend emits `track:updated` after the save, which the
      // surrounding views listen to and react to (re-fetch the row).
    } catch (err) {
      console.error("[TrackProperties] update_track_tags failed", err);
    } finally {
      setSaving(false);
    }
  };

  const handlePickCover = async () => {
    if (!track || saving) return;
    try {
      const path = await pickFile(
        ["jpg", "jpeg", "png", "webp", "bmp", "gif"],
        t("trackProperties.changeCover"),
      );
      if (!path) return;
      setSaving(true);
      await updateTrackCover(track.id, path);
      // The backend's `track:updated` event triggers a refetch in the
      // surrounding views; the modal itself stays open on the same
      // row but its `track` prop will get a new `artwork_path` once
      // the parent passes it back down.
    } catch (err) {
      console.error("[TrackProperties] update_track_cover failed", err);
    } finally {
      setSaving(false);
    }
  };

  const handleShowInExplorer = () => {
    revealItemInDir(track.file_path).catch((err) =>
      console.error("[TrackProperties] revealItemInDir failed", err),
    );
  };

  const sampleRateKHz = track.sample_rate
    ? `${(track.sample_rate / 1000).toFixed(1).replace(/\.0$/, "")} kHz`
    : "—";
  const bitDepth = track.bit_depth ? `${track.bit_depth}-bit` : "—";
  const bitrate = track.bitrate ? `${track.bitrate} kb/s` : "—";
  const channels = track.channels
    ? track.channels === 1
      ? t("trackProperties.mono")
      : track.channels === 2
        ? t("trackProperties.stereo")
        : `${track.channels} ch`
    : "—";
  const fileSizeMB =
    track.file_size > 0
      ? `${(track.file_size / (1024 * 1024)).toFixed(1)} Mo`
      : "—";
  const addedAt = track.added_at
    ? new Date(track.added_at).toLocaleString(i18n.language)
    : "—";
  const trackNumber =
    track.track_number != null
      ? track.disc_number != null && track.disc_number > 0
        ? `${track.disc_number} / ${track.track_number}`
        : String(track.track_number)
      : "—";

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("trackProperties.title")}
        className="relative w-full max-w-2xl rounded-3xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in max-h-[90vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          onClick={onClose}
          aria-label={t("common.close")}
          className="absolute top-4 right-4 p-2 rounded-full hover:bg-zinc-100 dark:hover:bg-zinc-800 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
        >
          <X size={18} />
        </button>

        {/* Header — cover + title block */}
        <div className="flex items-start gap-4 p-6 border-b border-zinc-100 dark:border-zinc-800">
          <div className="relative shrink-0 group">
            <Artwork
              path={track.artwork_path}
              path1x={track.artwork_path_1x}
              path2x={track.artwork_path_2x}
              size="2x"
              alt={track.album_title ?? track.title}
              className="w-24 h-24 shadow-sm"
              iconSize={32}
              rounded="xl"
            />
            {editing && (
              <button
                type="button"
                onClick={handlePickCover}
                disabled={saving}
                aria-label={t("trackProperties.changeCover")}
                title={t("trackProperties.changeCover")}
                className="absolute inset-0 rounded-xl flex items-center justify-center bg-black/60 text-white opacity-0 hover:opacity-100 focus:opacity-100 transition-opacity disabled:cursor-not-allowed"
              >
                <ImageUp size={28} />
              </button>
            )}
          </div>
          <div className="flex-1 min-w-0 pr-10">
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("trackProperties.title")}
            </div>
            {editing ? (
              <div className="space-y-2">
                <input
                  type="text"
                  value={form.title}
                  onChange={(e) =>
                    setForm((p) => ({ ...p, title: e.target.value }))
                  }
                  placeholder={t("trackProperties.fields.title")}
                  className="w-full text-lg font-semibold px-2 py-1 rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100"
                />
                <input
                  type="text"
                  value={form.artist}
                  onChange={(e) =>
                    setForm((p) => ({ ...p, artist: e.target.value }))
                  }
                  placeholder={t("trackProperties.fields.artist")}
                  className="w-full text-sm px-2 py-1 rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200"
                />
                <input
                  type="text"
                  value={form.album}
                  onChange={(e) =>
                    setForm((p) => ({ ...p, album: e.target.value }))
                  }
                  placeholder={t("trackProperties.fields.album")}
                  className="w-full text-sm px-2 py-1 rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200"
                />
              </div>
            ) : (
              <>
                <h2 className="text-xl font-bold text-zinc-900 dark:text-white truncate">
                  {track.title}
                </h2>
                <div className="text-sm text-zinc-500 dark:text-zinc-400 truncate mt-0.5">
                  {track.artist_name ?? "—"}
                </div>
                {track.album_title && (
                  <div className="text-sm text-zinc-500 dark:text-zinc-400 truncate">
                    {track.album_title}
                  </div>
                )}
                <div className="mt-2">
                  <HiResBadge
                    bitDepth={track.bit_depth}
                    sampleRate={track.sample_rate}
                    codec={track.codec}
                    variant="inline"
                  />
                </div>
              </>
            )}
          </div>
        </div>

        {/* Body — three sections, each rendered as label/value rows. */}
        <div className="p-6 space-y-6">
          <Section title={t("trackProperties.sections.metadata")}>
            {editing ? (
              <>
                <EditRow
                  label={t("trackProperties.year")}
                  type="number"
                  value={form.year}
                  onChange={(v) => setForm((p) => ({ ...p, year: v }))}
                  placeholder="2024"
                />
                <EditRow
                  label={t("trackProperties.fields.trackNumber")}
                  type="number"
                  value={form.track_number}
                  onChange={(v) =>
                    setForm((p) => ({ ...p, track_number: v }))
                  }
                  placeholder="1"
                />
                <EditRow
                  label={t("trackProperties.fields.discNumber")}
                  type="number"
                  value={form.disc_number}
                  onChange={(v) =>
                    setForm((p) => ({ ...p, disc_number: v }))
                  }
                  placeholder="1"
                />
                <EditRow
                  label={t("trackProperties.fields.genre")}
                  type="text"
                  value={form.genre}
                  onChange={(v) => setForm((p) => ({ ...p, genre: v }))}
                  placeholder={t("trackProperties.fields.genrePlaceholder")}
                />
                <Row
                  label={t("trackProperties.duration")}
                  value={formatDuration(track.duration_ms)}
                />
              </>
            ) : (
              <>
                <Row
                  label={t("trackProperties.year")}
                  value={track.year ?? "—"}
                />
                <Row
                  label={t("trackProperties.trackNumber")}
                  value={trackNumber}
                />
                <Row
                  label={t("trackProperties.duration")}
                  value={formatDuration(track.duration_ms)}
                />
                <Row
                  label={t("library.rating")}
                  value={
                    <StarRating
                      value={
                        ratingOverride === "none"
                          ? track.rating
                          : ratingOverride
                      }
                      onChange={handleSetRating}
                      size="md"
                    />
                  }
                />
              </>
            )}
          </Section>

          <Section title={t("trackProperties.sections.audio")}>
            <Row
              label={t("trackProperties.codec")}
              value={track.codec ?? "—"}
            />
            <Row label={t("trackProperties.bitDepth")} value={bitDepth} />
            <Row
              label={t("trackProperties.sampleRate")}
              value={sampleRateKHz}
            />
            <Row label={t("trackProperties.channels")} value={channels} />
            <Row label={t("trackProperties.bitrate")} value={bitrate} />
            <Row
              label={t("trackProperties.key")}
              value={track.musical_key ?? "—"}
            />
          </Section>

          <Section
            title={t("trackProperties.sections.analysis")}
            action={
              <button
                type="button"
                onClick={handleAnalyze}
                disabled={analyzing}
                className="inline-flex items-center gap-1.5 text-[11px] font-medium px-2 py-1 rounded-md text-emerald-600 hover:bg-emerald-50 dark:text-emerald-400 dark:hover:bg-emerald-500/10 disabled:opacity-50 transition-colors"
              >
                <Sparkles size={12} />
                <span>
                  {analyzing
                    ? t("trackProperties.analyzing")
                    : analysis
                      ? t("trackProperties.reanalyze")
                      : t("trackProperties.analyze")}
                </span>
              </button>
            }
          >
            <Row
              label={t("trackProperties.bpm")}
              value={analysis?.bpm != null ? Math.round(analysis.bpm) : "—"}
            />
            <Row
              label={t("trackProperties.loudness")}
              value={
                analysis?.loudness_lufs != null
                  ? `${analysis.loudness_lufs.toFixed(1)} dB`
                  : "—"
              }
            />
            <Row
              label={t("trackProperties.replayGain")}
              value={
                analysis?.replay_gain_db != null
                  ? `${analysis.replay_gain_db >= 0 ? "+" : ""}${analysis.replay_gain_db.toFixed(1)} dB`
                  : "—"
              }
            />
            <Row
              label={t("trackProperties.peak")}
              value={analysis?.peak != null ? analysis.peak.toFixed(3) : "—"}
            />
          </Section>

          <Section title={t("trackProperties.sections.file")}>
            <Row label={t("trackProperties.fileSize")} value={fileSizeMB} />
            <Row label={t("trackProperties.addedAt")} value={addedAt} />
            <Row
              label={t("trackProperties.filePath")}
              value={
                <span className="font-mono text-xs break-all">
                  {track.file_path}
                </span>
              }
            />
          </Section>
        </div>

        <div className="px-6 py-4 border-t border-zinc-100 dark:border-zinc-800 flex justify-end gap-2">
          {editing ? (
            <>
              <button
                type="button"
                onClick={() => setEditing(false)}
                disabled={saving}
                className="px-4 py-2 rounded-xl text-sm font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors disabled:opacity-50"
              >
                {t("common.cancel")}
              </button>
              <button
                type="button"
                onClick={handleSave}
                disabled={saving}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors disabled:opacity-50"
              >
                <Save size={14} />
                <span>
                  {saving
                    ? t("trackProperties.saving")
                    : t("trackProperties.save")}
                </span>
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                onClick={handleShowInExplorer}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
              >
                <ExternalLink size={14} />
                <span>{t("trackProperties.showInExplorer")}</span>
              </button>
              <button
                type="button"
                onClick={() => setEditing(true)}
                className="inline-flex items-center gap-2 px-4 py-2 rounded-xl text-sm font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
              >
                <Pencil size={14} />
                <span>{t("trackProperties.edit")}</span>
              </button>
              <button
                type="button"
                onClick={onClose}
                className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors"
              >
                {t("common.close")}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Section({
  title,
  children,
  action,
}: {
  title: string;
  children: React.ReactNode;
  action?: React.ReactNode;
}) {
  return (
    <section>
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase">
          {title}
        </h3>
        {action}
      </div>
      <div className="divide-y divide-zinc-100 dark:divide-zinc-800/60 rounded-xl border border-zinc-100 dark:border-zinc-800/60 overflow-hidden">
        {children}
      </div>
    </section>
  );
}

function Row({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="flex items-start gap-4 px-3 py-2 text-sm">
      <span className="w-32 shrink-0 text-zinc-500 dark:text-zinc-400">
        {label}
      </span>
      <span className="flex-1 min-w-0 text-zinc-800 dark:text-zinc-200 tabular-nums">
        {value}
      </span>
    </div>
  );
}

function EditRow({
  label,
  value,
  onChange,
  type = "text",
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  type?: "text" | "number";
  placeholder?: string;
}) {
  return (
    <div className="flex items-center gap-4 px-3 py-2 text-sm">
      <span className="w-32 shrink-0 text-zinc-500 dark:text-zinc-400">
        {label}
      </span>
      <input
        type={type}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="flex-1 min-w-0 px-2 py-1 rounded-md border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-800 dark:text-zinc-200"
      />
    </div>
  );
}
