import { useCallback, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Pencil, Save, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  updateTracksBatch,
  type BatchUpdateSummary,
  type TrackEdit,
} from "../../lib/tauri/track";

interface BatchTagEditModalProps {
  /** IDs of the tracks to batch-edit. `null` keeps the modal closed. */
  trackIds: number[] | null;
  onClose: () => void;
}

/**
 * Fields the batch editor exposes. Title / track_number / disc_number
 * are deliberately excluded — they're per-track-unique and bulk-editing
 * them across a selection would silently overwrite legitimate values
 * (e.g. every track in an album sharing the same track number).
 *
 * Each field carries an `enabled` flag so the user can opt fields in
 * individually: untouched fields stay `enabled=false`, the modal sends
 * only enabled fields to the backend, and the backend treats the
 * remaining fields as `None` (= leave untouched).
 */
type FieldId = "artist" | "album" | "year" | "genre";

interface FieldState {
  enabled: boolean;
  value: string;
}

const FIELD_CONFIG: { id: FieldId; type: "text" | "number" }[] = [
  { id: "artist", type: "text" },
  { id: "album", type: "text" },
  { id: "year", type: "number" },
  { id: "genre", type: "text" },
];

function blank(): Record<FieldId, FieldState> {
  return {
    artist: { enabled: false, value: "" },
    album: { enabled: false, value: "" },
    year: { enabled: false, value: "" },
    genre: { enabled: false, value: "" },
  };
}

export function BatchTagEditModal({
  trackIds,
  onClose,
}: BatchTagEditModalProps) {
  const { t } = useTranslation();
  const isOpen = trackIds != null && trackIds.length > 0;
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  const [fields, setFields] = useState<Record<FieldId, FieldState>>(blank);
  const [isSaving, setIsSaving] = useState(false);
  const [summary, setSummary] = useState<BatchUpdateSummary | null>(null);

  const enabledCount = useMemo(
    () => Object.values(fields).filter((f) => f.enabled).length,
    [fields],
  );

  const handleToggle = useCallback((id: FieldId) => {
    setFields((prev) => ({
      ...prev,
      [id]: { ...prev[id], enabled: !prev[id].enabled },
    }));
  }, []);

  const handleChange = useCallback((id: FieldId, value: string) => {
    setFields((prev) => ({
      ...prev,
      [id]: { enabled: true, value },
    }));
  }, []);

  const handleClose = useCallback(() => {
    if (isSaving) return;
    setFields(blank());
    setSummary(null);
    onClose();
  }, [isSaving, onClose]);

  const handleSave = useCallback(async () => {
    if (!trackIds || trackIds.length === 0 || enabledCount === 0) return;
    setIsSaving(true);
    setSummary(null);
    try {
      const edit: TrackEdit = {};
      if (fields.artist.enabled) edit.artist = fields.artist.value.trim();
      if (fields.album.enabled) edit.album = fields.album.value.trim();
      if (fields.year.enabled) {
        const parsed = parseInt(fields.year.value, 10);
        edit.year = Number.isFinite(parsed) ? parsed : 0;
      }
      if (fields.genre.enabled) edit.genre = fields.genre.value.trim();
      const result = await updateTracksBatch(trackIds, edit);
      setSummary(result);
      if (result.errors.length === 0) {
        // Clean exit — close after a short delay so the success line is
        // legible. Errors keep the modal open so the user can read them.
        window.setTimeout(() => handleClose(), 700);
      }
    } catch (err) {
      console.error("[BatchTagEditModal] update_tracks_batch failed", err);
      setSummary({
        updated: 0,
        errors: [[-1, String(err)]],
      });
    } finally {
      setIsSaving(false);
    }
  }, [trackIds, fields, enabledCount, handleClose]);

  if (!isOpen || trackIds == null) return null;

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={handleClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="batch-tag-edit-title"
        className="relative w-full max-w-lg rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in max-h-[90vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4 mb-5">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-emerald-500/10 text-emerald-500 flex items-center justify-center">
              <Pencil size={18} />
            </div>
            <div>
              <h2
                id="batch-tag-edit-title"
                className="text-lg font-bold text-zinc-900 dark:text-white"
              >
                {t("batchTagEdit.title")}
              </h2>
              <p className="text-xs text-zinc-500">
                {t("batchTagEdit.subtitle", { count: trackIds.length })}
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("common.close")}
            className="p-2 rounded-full text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4">
          {t("batchTagEdit.help")}
        </p>

        <div className="space-y-3">
          {FIELD_CONFIG.map(({ id, type }) => {
            const state = fields[id];
            return (
              <label
                key={id}
                className={`flex items-start gap-3 p-3 rounded-xl border transition-colors cursor-pointer ${
                  state.enabled
                    ? "border-emerald-300 bg-emerald-50/50 dark:border-emerald-700 dark:bg-emerald-900/10"
                    : "border-zinc-200 dark:border-zinc-700"
                }`}
              >
                <input
                  type="checkbox"
                  checked={state.enabled}
                  onChange={() => handleToggle(id)}
                  className="mt-1 accent-emerald-500"
                />
                <div className="flex-1 min-w-0">
                  <div className="text-xs font-semibold tracking-wider uppercase text-zinc-500 mb-1">
                    {t(`batchTagEdit.fields.${id}`)}
                  </div>
                  <input
                    type={type}
                    value={state.value}
                    onChange={(e) => handleChange(id, e.currentTarget.value)}
                    onFocus={() => {
                      if (!state.enabled) handleToggle(id);
                    }}
                    placeholder={t(`batchTagEdit.placeholders.${id}`)}
                    disabled={!state.enabled}
                    className="w-full px-2 py-1.5 rounded-md text-sm bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-emerald-500"
                  />
                </div>
              </label>
            );
          })}
        </div>

        {summary && (
          <div className="mt-4 p-3 rounded-xl bg-zinc-50 dark:bg-zinc-800/40 text-xs space-y-1">
            <div className="font-semibold text-zinc-700 dark:text-zinc-200">
              {t("batchTagEdit.result.updated", { count: summary.updated })}
            </div>
            {summary.errors.length > 0 && (
              <>
                <div className="text-red-600 dark:text-red-400">
                  {t("batchTagEdit.result.failed", {
                    count: summary.errors.length,
                  })}
                </div>
                <ul className="max-h-32 overflow-y-auto text-zinc-500 dark:text-zinc-400 list-disc list-inside">
                  {summary.errors.map(([id, msg]) => (
                    <li key={id}>
                      #{id}: {msg}
                    </li>
                  ))}
                </ul>
              </>
            )}
          </div>
        )}

        <div className="flex items-center justify-end gap-2 mt-5">
          <button
            type="button"
            onClick={handleClose}
            disabled={isSaving}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={isSaving || enabledCount === 0}
            className="px-5 py-2 rounded-xl text-sm font-semibold text-white bg-emerald-500 hover:bg-emerald-600 shadow-lg transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isSaving ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <Save size={14} />
            )}
            <span>
              {t("batchTagEdit.submit", {
                count: enabledCount,
              })}
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}
