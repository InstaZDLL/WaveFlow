import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Sparkles, Loader2, Eye } from "lucide-react";
import {
  createCustomSmartPlaylist,
  previewCustomSmartPlaylist,
  updateCustomSmartPlaylist,
  emptyTree,
  type CustomRules,
  type CustomSort,
  type RuleNode,
} from "../../lib/tauri/smart_playlists";
import { listGenres, type GenreRow } from "../../lib/tauri/browse";
import { useModalA11y } from "../../hooks/useModalA11y";
import { RuleTreeEditor } from "./RuleTreeEditor";

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

/**
 * Smart playlist editor with a recursive boolean rule tree
 * (AND / OR / NOT / leaf predicates). Live preview shows how many
 * tracks the current tree matches without persisting anything.
 *
 * The existing-rule path auto-migrates the v1 flat shape on read
 * (backend deserializer handles that), so opening an old playlist
 * shows its predicates as a flat `All` of leaves — the user can then
 * re-organise into nested groups.
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
  const [tree, setTree] = useState<RuleNode>(emptyTree());
  const [sort, setSort] = useState<CustomSort>("added_desc");
  const [limit, setLimit] = useState<number | null>(null);
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
      // The backend's deserializer migrates v1 → v2 transparently, so
      // `existing.rules.tree` is always present here.
      setTree(existing.rules.tree ?? emptyTree());
      setSort(existing.rules.sort ?? "added_desc");
      setLimit(existing.rules.limit ?? null);
    } else {
      setName("");
      setDescription("");
      setTree(emptyTree());
      setSort("added_desc");
      setLimit(null);
    }
    /* eslint-enable react-hooks/set-state-in-effect */
    listGenres(null)
      .then(setGenres)
      .catch(() => {});
  }, [isOpen, existing]);

  if (!isOpen) return null;

  const currentRules = (): CustomRules => ({ tree, sort, limit });

  const handlePreview = async () => {
    setIsPreviewing(true);
    setError(null);
    try {
      const result = await previewCustomSmartPlaylist(currentRules());
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
        rules: currentRules(),
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
        className="relative bg-white dark:bg-surface-dark-elevated text-zinc-900 dark:text-zinc-100 rounded-3xl border border-zinc-200 dark:border-zinc-800 shadow-2xl w-full max-w-3xl max-h-[90vh] flex flex-col overflow-hidden animate-fade-in"
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

          {/* Rule tree */}
          <Section
            title={t("smartPlaylistEditor.tree.sectionTitle")}
            hint={t("smartPlaylistEditor.tree.sectionHint")}
          >
            <RuleTreeEditor
              root={tree}
              onChange={(next) => {
                setTree(next);
                setPreviewCount(null);
              }}
              genres={genres}
            />
          </Section>

          {/* Sort + limit */}
          <Section title={t("smartPlaylistEditor.sections.output")}>
            <Field label={t("smartPlaylistEditor.fields.sort")}>
              <select
                value={sort}
                onChange={(e) => {
                  setSort(e.target.value as CustomSort);
                  setPreviewCount(null);
                }}
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
                value={limit ?? ""}
                onChange={(e) => {
                  setLimit(e.target.value ? Number(e.target.value) : null);
                  setPreviewCount(null);
                }}
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
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-3">
      <div className="border-b border-zinc-200 dark:border-zinc-800 pb-2">
        <h3 className="text-sm font-semibold text-zinc-700 dark:text-zinc-300">
          {title}
        </h3>
        {hint && (
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-1">
            {hint}
          </p>
        )}
      </div>
      {children}
    </div>
  );
}
