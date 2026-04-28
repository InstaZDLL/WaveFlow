import { useTranslation } from "react-i18next";
import { ExternalLink, X } from "lucide-react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Artwork } from "./Artwork";
import { HiResBadge } from "./HiResBadge";
import { formatDuration, type Track } from "../../lib/tauri/track";

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
  track,
  onClose,
}: TrackPropertiesModalProps) {
  const { t, i18n } = useTranslation();
  if (!track) return null;

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
          <Artwork
            path={track.artwork_path}
            path1x={track.artwork_path_1x}
            path2x={track.artwork_path_2x}
            size="2x"
            alt={track.album_title ?? track.title}
            className="w-24 h-24 shrink-0 shadow-sm"
            iconSize={32}
            rounded="xl"
          />
          <div className="flex-1 min-w-0 pr-10">
            <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
              {t("trackProperties.title")}
            </div>
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
                variant="inline"
              />
            </div>
          </div>
        </div>

        {/* Body — three sections, each rendered as label/value rows. */}
        <div className="p-6 space-y-6">
          <Section title={t("trackProperties.sections.metadata")}>
            <Row label={t("trackProperties.year")} value={track.year ?? "—"} />
            <Row label={t("trackProperties.trackNumber")} value={trackNumber} />
            <Row
              label={t("trackProperties.duration")}
              value={formatDuration(track.duration_ms)}
            />
          </Section>

          <Section title={t("trackProperties.sections.audio")}>
            <Row label={t("trackProperties.codec")} value={track.codec ?? "—"} />
            <Row label={t("trackProperties.bitDepth")} value={bitDepth} />
            <Row label={t("trackProperties.sampleRate")} value={sampleRateKHz} />
            <Row label={t("trackProperties.channels")} value={channels} />
            <Row label={t("trackProperties.bitrate")} value={bitrate} />
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
            onClick={onClose}
            className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 transition-colors"
          >
            {t("common.close")}
          </button>
        </div>
      </div>
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h3 className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2">
        {title}
      </h3>
      <div className="divide-y divide-zinc-100 dark:divide-zinc-800/60 rounded-xl border border-zinc-100 dark:border-zinc-800/60 overflow-hidden">
        {children}
      </div>
    </section>
  );
}

function Row({
  label,
  value,
}: {
  label: string;
  value: React.ReactNode;
}) {
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
