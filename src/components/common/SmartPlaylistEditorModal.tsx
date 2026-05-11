import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Sparkles, Loader2, Eye } from "lucide-react";
import {
  createCustomSmartPlaylist,
  previewCustomSmartPlaylist,
  updateCustomSmartPlaylist,
  type CustomRules,
  type CustomSort,
} from "../../lib/tauri/smart_playlists";
import { listGenres, type GenreRow } from "../../lib/tauri/browse";
import { useModalA11y } from "../../hooks/useModalA11y";

interface SmartPlaylistEditorModalProps {
  isOpen: boolean;
  onClose: () => void;
  /** When provided, the modal switches to edit mode for this playlist. */
  existing?: {
    id: number;
    name: string;
    description?: string | null;
    rules: CustomRules;
  } | null;
  /** Called after a successful save with the playlist id + track count. */
  onSaved?: (playlistId: number, trackCount: number) => void;
}

const SORT_OPTIONS: { value: CustomSort; key: string }[] = [
  { value: "added_desc", key: "addedDesc" },
  { value: "added_asc", key: "addedAsc" },
  { value: "year_desc", key: "yearDesc" },
  { value: "year_asc", key: "yearAsc" },
  { value: "title_asc", key: "titleAsc" },
  { value: "artist_asc", key: "artistAsc" },
  { value: "random", key: "random" },
];

const FORMAT_OPTIONS = ["FLAC", "MP3", "AAC", "OGG", "OPUS", "WAV", "DSF", "DFF"];

/**
 * Rule-driven smart playlist editor. Mirrors the search filter shape
 * but persists as a `playlist.smart_rules` JSON blob. Live preview
 * shows how many tracks the current rule set matches without
 * persisting anything.
 */
export function SmartPlaylistEditorModal({
  isOpen,
  onClose,
  existing,
  onSaved,
}: SmartPlaylistEditorModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [rules, setRules] = useState<CustomRules>({});
  const [genres, setGenres] = useState<GenreRow[]>([]);
  const [previewCount, setPreviewCount] = useState<number | null>(null);
  const [isPreviewing, setIsPreviewing] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  // ── Hydrate on open ─────────────────────────────────────────────
  useEffect(() => {
    if (!isOpen) return;
    /* eslint-disable react-hooks/set-state-in-effect */
    setError(null);
    setPreviewCount(null);
    if (existing) {
      setName(existing.name);
      setDescription(existing.description ?? "");
      setRules(existing.rules);
    } else {
      setName("");
      setDescription("");
      setRules({ sort: "added_desc" });
    }
    /* eslint-enable react-hooks/set-state-in-effect */
    listGenres(null).then(setGenres).catch(() => {});
  }, [isOpen, existing]);

  if (!isOpen) return null;

  const updateRule = <K extends keyof CustomRules>(
    key: K,
    value: CustomRules[K] | null,
  ) => {
    setRules((prev) => ({ ...prev, [key]: value }));
    setPreviewCount(null);
  };

  const toggleGenre = (id: number) => {
    const current = new Set(rules.genre_ids ?? []);
    if (current.has(id)) current.delete(id);
    else current.add(id);
    updateRule("genre_ids", current.size ? Array.from(current) : null);
  };

  const toggleFormat = (fmt: string) => {
    const current = new Set(rules.formats ?? []);
    if (current.has(fmt)) current.delete(fmt);
    else current.add(fmt);
    updateRule("formats", current.size ? Array.from(current) : null);
  };

  const handlePreview = async () => {
    setIsPreviewing(true);
    setError(null);
    try {
      const result = await previewCustomSmartPlaylist(rules);
      setPreviewCount(result.total);
    } catch (err) {
      console.error("[SmartPlaylistEditor] preview failed", err);
      setError(String(err));
    } finally {
      setIsPreviewing(false);
    }
  };

  const handleSave = async () => {
    if (!name.trim()) {
      setError(t("smartPlaylistEditor.nameRequired"));
      return;
    }
    setIsSaving(true);
    setError(null);
    try {
      const input = {
        name: name.trim(),
        description: description.trim() || null,
        icon_id: "sparkles" as const,
        rules,
      };
      const result = existing
        ? await updateCustomSmartPlaylist(existing.id, input)
        : await createCustomSmartPlaylist(input);
      onSaved?.(result.playlist_id, result.track_count);
      onClose();
    } catch (err) {
      console.error("[SmartPlaylistEditor] save failed", err);
      setError(String(err));
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="smart-playlist-editor-title"
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-2xl max-h-[90vh] flex flex-col overflow-hidden animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-6 py-4 border-b border-zinc-200 dark:border-zinc-800">
          <div className="flex items-center gap-2">
            <Sparkles size={18} className="text-violet-500" />
            <h2
              id="smart-playlist-editor-title"
              className="text-lg font-semibold"
            >
              {existing
                ? t("smartPlaylistEditor.editTitle")
                : t("smartPlaylistEditor.createTitle")}
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
            aria-label={t("common.close")}
          >
            <X size={18} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto p-6 space-y-5">
          {/* Identity */}
          <div className="space-y-3">
            <Field label={t("smartPlaylistEditor.fields.name")}>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={t("smartPlaylistEditor.placeholders.name")}
                className={inputClass}
                autoFocus
              />
            </Field>
            <Field label={t("smartPlaylistEditor.fields.description")}>
              <input
                type="text"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                placeholder={t("smartPlaylistEditor.placeholders.description")}
                className={inputClass}
              />
            </Field>
          </div>

          {/* Text filters */}
          <Section title={t("smartPlaylistEditor.sections.match")}>
            <Field label={t("smartPlaylistEditor.fields.title")}>
              <input
                type="text"
                value={rules.title_contains ?? ""}
                onChange={(e) =>
                  updateRule("title_contains", e.target.value || null)
                }
                className={inputClass}
              />
            </Field>
            <Field label={t("smartPlaylistEditor.fields.artist")}>
              <input
                type="text"
                value={rules.artist_contains ?? ""}
                onChange={(e) =>
                  updateRule("artist_contains", e.target.value || null)
                }
                className={inputClass}
              />
            </Field>
            <Field label={t("smartPlaylistEditor.fields.album")}>
              <input
                type="text"
                value={rules.album_contains ?? ""}
                onChange={(e) =>
                  updateRule("album_contains", e.target.value || null)
                }
                className={inputClass}
              />
            </Field>
          </Section>

          {/* Numeric ranges */}
          <Section title={t("smartPlaylistEditor.sections.ranges")}>
            <NumericRange
              label={t("smartPlaylistEditor.fields.year")}
              min={rules.year_min ?? null}
              max={rules.year_max ?? null}
              onMinChange={(v) => updateRule("year_min", v)}
              onMaxChange={(v) => updateRule("year_max", v)}
              step={1}
            />
            <NumericRange
              label={t("smartPlaylistEditor.fields.bpm")}
              min={rules.bpm_min ?? null}
              max={rules.bpm_max ?? null}
              onMinChange={(v) => updateRule("bpm_min", v)}
              onMaxChange={(v) => updateRule("bpm_max", v)}
              step={1}
              isFloat
            />
            <NumericRange
              label={t("smartPlaylistEditor.fields.durationMin")}
              min={msToMin(rules.duration_min_ms)}
              max={msToMin(rules.duration_max_ms)}
              onMinChange={(v) =>
                updateRule("duration_min_ms", v == null ? null : v * 60_000)
              }
              onMaxChange={(v) =>
                updateRule("duration_max_ms", v == null ? null : v * 60_000)
              }
              step={1}
            />
          </Section>

          {/* Toggles + multi-select */}
          <Section title={t("smartPlaylistEditor.sections.attributes")}>
            <div className="flex flex-wrap gap-3">
              <Toggle
                checked={rules.hi_res_only === true}
                onChange={(v) => updateRule("hi_res_only", v ? true : null)}
                label={t("smartPlaylistEditor.fields.hiRes")}
              />
              <Toggle
                checked={rules.liked_only === true}
                onChange={(v) => updateRule("liked_only", v ? true : null)}
                label={t("smartPlaylistEditor.fields.liked")}
              />
            </div>
            <Field label={t("smartPlaylistEditor.fields.formats")}>
              <div className="flex flex-wrap gap-2">
                {FORMAT_OPTIONS.map((fmt) => {
                  const active = (rules.formats ?? []).includes(fmt);
                  return (
                    <button
                      type="button"
                      key={fmt}
                      onClick={() => toggleFormat(fmt)}
                      className={`px-3 py-1.5 rounded-full text-xs font-medium transition-colors ${
                        active
                          ? "bg-violet-500 text-white"
                          : "bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                      }`}
                    >
                      {fmt}
                    </button>
                  );
                })}
              </div>
            </Field>
            {genres.length > 0 && (
              <Field label={t("smartPlaylistEditor.fields.genres")}>
                <div className="flex flex-wrap gap-2 max-h-32 overflow-y-auto">
                  {genres.map((g) => {
                    const active = (rules.genre_ids ?? []).includes(g.id);
                    return (
                      <button
                        type="button"
                        key={g.id}
                        onClick={() => toggleGenre(g.id)}
                        className={`px-3 py-1.5 rounded-full text-xs transition-colors ${
                          active
                            ? "bg-violet-500 text-white"
                            : "bg-zinc-100 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-300 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                        }`}
                      >
                        {g.name}
                      </button>
                    );
                  })}
                </div>
              </Field>
            )}
          </Section>

          {/* Sort + limit */}
          <Section title={t("smartPlaylistEditor.sections.output")}>
            <Field label={t("smartPlaylistEditor.fields.sort")}>
              <select
                value={rules.sort ?? "added_desc"}
                onChange={(e) =>
                  updateRule("sort", e.target.value as CustomSort)
                }
                className={inputClass}
              >
                {SORT_OPTIONS.map((s) => (
                  <option key={s.value} value={s.value}>
                    {t(`smartPlaylistEditor.sortOptions.${s.key}`)}
                  </option>
                ))}
              </select>
            </Field>
            <Field label={t("smartPlaylistEditor.fields.limit")}>
              <input
                type="number"
                min={1}
                max={5000}
                value={rules.limit ?? ""}
                onChange={(e) =>
                  updateRule(
                    "limit",
                    e.target.value ? Number(e.target.value) : null,
                  )
                }
                placeholder={t("smartPlaylistEditor.placeholders.noLimit")}
                className={inputClass}
              />
            </Field>
          </Section>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between gap-3 px-6 py-4 border-t border-zinc-200 dark:border-zinc-800">
          <button
            type="button"
            onClick={handlePreview}
            disabled={isPreviewing}
            className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors flex items-center gap-2"
          >
            {isPreviewing ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <Eye size={14} />
            )}
            {previewCount != null
              ? t("smartPlaylistEditor.previewCount", { count: previewCount })
              : t("smartPlaylistEditor.preview")}
          </button>
          <div className="flex items-center gap-2">
            {error && (
              <span className="text-xs text-red-500 truncate max-w-xs">
                {error}
              </span>
            )}
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 rounded-full text-sm hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              onClick={handleSave}
              disabled={isSaving}
              className="px-5 py-2 rounded-full bg-zinc-900 dark:bg-white text-white dark:text-zinc-900 text-sm font-medium hover:opacity-90 disabled:opacity-50 transition-opacity flex items-center gap-2"
            >
              {isSaving && <Loader2 size={14} className="animate-spin" />}
              {existing ? t("common.save") : t("smartPlaylistEditor.create")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

const inputClass =
  "w-full px-3 py-2 rounded-lg border border-zinc-200 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-800 text-sm focus:outline-none focus:ring-2 focus:ring-violet-500";

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="block">
      <span className="block text-xs font-medium text-zinc-500 dark:text-zinc-400 mb-1.5 uppercase tracking-wide">
        {label}
      </span>
      {children}
    </label>
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
    <div className="space-y-3">
      <h3 className="text-sm font-semibold text-zinc-700 dark:text-zinc-300 border-b border-zinc-200 dark:border-zinc-800 pb-2">
        {title}
      </h3>
      {children}
    </div>
  );
}

function Toggle({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
}) {
  return (
    <label className="flex items-center gap-2 text-sm cursor-pointer select-none px-3 py-1.5 rounded-full bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 transition-colors">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="rounded"
      />
      {label}
    </label>
  );
}

function NumericRange({
  label,
  min,
  max,
  onMinChange,
  onMaxChange,
  step,
  isFloat,
}: {
  label: string;
  min: number | null;
  max: number | null;
  onMinChange: (v: number | null) => void;
  onMaxChange: (v: number | null) => void;
  step: number;
  isFloat?: boolean;
}) {
  const parse = (raw: string): number | null => {
    if (!raw) return null;
    const n = isFloat ? Number(raw) : parseInt(raw, 10);
    return Number.isFinite(n) ? n : null;
  };
  return (
    <Field label={label}>
      <div className="flex items-center gap-2">
        <input
          type="number"
          step={step}
          value={min ?? ""}
          onChange={(e) => onMinChange(parse(e.target.value))}
          placeholder="min"
          className={inputClass}
        />
        <span className="text-zinc-400">→</span>
        <input
          type="number"
          step={step}
          value={max ?? ""}
          onChange={(e) => onMaxChange(parse(e.target.value))}
          placeholder="max"
          className={inputClass}
        />
      </div>
    </Field>
  );
}

function msToMin(ms: number | null | undefined): number | null {
  if (ms == null) return null;
  return Math.round(ms / 60_000);
}
