import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, Archive, Loader2 } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import { formatBytes } from "../../lib/format";
import type {
  ArchiveCompatibility,
  ArchivePreview,
} from "../../lib/tauri/profile_io";

/** Where the preview stands: being read, read, or not readable at all. */
export type ArchivePreviewState =
  | { status: "loading" }
  | { status: "ready"; preview: ArchivePreview }
  | { status: "unreadable" }
  | { status: "failed" };

interface ProfileArchivePreviewModalProps {
  state: ArchivePreviewState | null;
  onCancel: () => void;
  onImport: () => void;
}

/** The sentence that says why an archive cannot be imported, or `null`
 * when it can. */
function refusalKey(compatibility: ArchiveCompatibility): string | null {
  switch (compatibility.kind) {
    case "compatible":
      return null;
    case "newerArchive":
    case "newerSchema":
      return "settings.profileIo.preview.newer";
    case "differentBuild":
      return "settings.profileIo.preview.differentBuild";
    case "unsupportedArchive":
      return "settings.profileIo.preview.unsupported";
  }
}

/**
 * Shows what a profile archive holds before anything is imported: its
 * profile, the WaveFlow version and database version that wrote it, and
 * what its database counts. Import is offered only when the backend says
 * the import would take the archive; an archive from a newer build or a
 * damaged file says so instead.
 */
export function ProfileArchivePreviewModal({
  state,
  onCancel,
  onImport,
}: ProfileArchivePreviewModalProps) {
  const { t, i18n } = useTranslation();
  const isOpen = state != null;
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onCancel);
  const titleId = useId();
  // The last state shown, kept while the dialog animates out so its
  // content does not blank under the closing animation.
  const [shown, setShown] = useState(state);
  if (state != null && state !== shown) setShown(state);
  const current = state ?? shown;

  const preview = current?.status === "ready" ? current.preview : null;
  const refusal = preview ? refusalKey(preview.compatibility) : null;
  const canImport = preview != null && refusal == null;
  const number = (n: number) => n.toLocaleString(i18n.language);

  const exportedAt = (() => {
    if (!preview) return "";
    const date = new Date(preview.exportedAt);
    return Number.isNaN(date.getTime())
      ? preview.exportedAt
      : date.toLocaleString(i18n.language, {
          dateStyle: "medium",
          timeStyle: "short",
        });
  })();

  const rows: [string, string][] = preview
    ? [
        ...(preview.contents
          ? ([
              ["tracks", number(preview.contents.tracks)],
              ["playlists", number(preview.contents.playlists)],
              ["smartPlaylists", number(preview.contents.smartPlaylists)],
              ["likedTracks", number(preview.contents.likedTracks)],
              ["plays", number(preview.contents.plays)],
              ["libraryFolders", number(preview.contents.libraryFolders)],
              ["settings", number(preview.contents.settings)],
            ] as [string, string][])
          : []),
        ...(preview.artworkFiles != null
          ? ([["covers", number(preview.artworkFiles)]] as [string, string][])
          : []),
        ...(preview.metadataArtworkFiles != null
          ? ([["coverCache", number(preview.metadataArtworkFiles)]] as [
              string,
              string,
            ][])
          : []),
        ["size", formatBytes(preview.sizeBytes, i18n.language)],
      ]
    : [];

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onCancel}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-busy={current?.status === "loading"}
        className="w-full max-w-md rounded-2xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 shadow-2xl overflow-hidden"
      >
        <div className="p-6 space-y-4">
          <div className="flex items-start gap-3">
            <div className="shrink-0 w-10 h-10 rounded-full bg-emerald-100 dark:bg-emerald-500/15 flex items-center justify-center">
              <Archive
                size={20}
                className="text-emerald-600 dark:text-emerald-400"
                aria-hidden="true"
              />
            </div>
            <div className="flex-1 min-w-0">
              <h2
                id={titleId}
                className="text-base font-semibold text-zinc-900 dark:text-white break-words"
              >
                {preview
                  ? t("settings.profileIo.preview.title", {
                      name: preview.profileName,
                    })
                  : t("settings.profileIo.preview.titleUnknown")}
              </h2>
              {preview && (
                <p className="mt-1 text-sm leading-5 text-zinc-600 dark:text-zinc-400">
                  {t("settings.profileIo.preview.origin", {
                    date: exportedAt,
                    version: preview.appVersion,
                  })}
                </p>
              )}
            </div>
          </div>

          {current?.status === "loading" && (
            <div
              role="status"
              className="flex items-center gap-2 text-sm text-zinc-600 dark:text-zinc-400"
            >
              <Loader2 size={14} className="animate-spin" aria-hidden="true" />
              {t("settings.profileIo.preview.loading")}
            </div>
          )}

          {(current?.status === "unreadable" ||
            current?.status === "failed") && (
            <div
              role="alert"
              className="flex items-start gap-2 text-sm text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-500/10 border border-red-200 dark:border-red-500/30 rounded-lg px-3 py-2"
            >
              <AlertTriangle
                size={16}
                className="shrink-0 mt-0.5"
                aria-hidden="true"
              />
              {current.status === "unreadable"
                ? t("settings.profileIo.preview.unreadable")
                : t("settings.profileIo.preview.failed")}
            </div>
          )}

          {preview && (
            <>
              <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1 text-sm">
                {rows.map(([key, value]) => (
                  <div key={key} className="contents">
                    <dt className="text-zinc-600 dark:text-zinc-400">
                      {t(`settings.profileIo.preview.rows.${key}`)}
                    </dt>
                    <dd className="text-right tabular-nums text-zinc-900 dark:text-white">
                      {value}
                    </dd>
                  </div>
                ))}
              </dl>
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                {preview.schemaVersion == null
                  ? t("settings.profileIo.preview.schemaUnknown", {
                      local: preview.localSchemaVersion,
                    })
                  : t("settings.profileIo.preview.schema", {
                      archive: preview.schemaVersion,
                      local: preview.localSchemaVersion,
                    })}
              </p>
              {refusal ? (
                <div
                  role="alert"
                  className="flex items-start gap-2 text-sm text-amber-700 dark:text-amber-400 bg-amber-50 dark:bg-amber-500/10 border border-amber-200 dark:border-amber-500/30 rounded-lg px-3 py-2"
                >
                  <AlertTriangle
                    size={16}
                    className="shrink-0 mt-0.5"
                    aria-hidden="true"
                  />
                  {t(refusal)}
                </div>
              ) : (
                <p className="text-xs text-zinc-500 dark:text-zinc-400">
                  {t("settings.profileIo.preview.asNewProfile")}
                </p>
              )}
            </>
          )}
        </div>

        <div className="px-6 py-4 bg-zinc-50 dark:bg-zinc-950/40 border-t border-zinc-200 dark:border-zinc-800 flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            className="px-4 py-2 text-sm font-medium rounded-lg text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("settings.profileIo.preview.cancel")}
          </button>
          <button
            type="button"
            onClick={onImport}
            disabled={!canImport}
            className="px-4 py-2 text-sm font-medium rounded-lg bg-emerald-600 hover:bg-emerald-700 text-white disabled:bg-emerald-600/40 disabled:cursor-not-allowed transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            {t("settings.profileIo.preview.import")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
