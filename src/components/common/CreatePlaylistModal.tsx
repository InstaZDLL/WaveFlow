import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Check } from "lucide-react";
import {
  PLAYLIST_COLORS,
  PLAYLIST_ICONS,
} from "../../lib/playlistVisuals";
import type { Playlist } from "../../lib/tauri/playlist";

interface CreatePlaylistModalProps {
  isOpen: boolean;
  onClose: () => void;
  /**
   * Submit handler. Called with the form data when the user clicks the
   * primary button. Edit mode is triggered by passing `existing` — the
   * handler then receives the original playlist's id via `existing.id`
   * and should invoke `updatePlaylist` instead of `createPlaylist`.
   */
  onCreate?: (data: {
    name: string;
    description: string;
    colorId: string;
    iconId: string;
  }) => void;
  /**
   * When provided, the modal switches to edit mode: title + button label
   * change, fields pre-fill from this playlist, and the submit action is
   * interpreted by the parent as an update.
   */
  existing?: Playlist | null;
}

export function CreatePlaylistModal({
  isOpen,
  onClose,
  onCreate,
  existing,
}: CreatePlaylistModalProps) {
  const { t } = useTranslation();
  const isEdit = existing != null;
  const [name, setName] = useState(existing?.name ?? "");
  const [description, setDescription] = useState(existing?.description ?? "");
  const [selectedColorId, setSelectedColorId] = useState(
    existing?.color_id ?? PLAYLIST_COLORS[0].id
  );
  const [selectedIconId, setSelectedIconId] = useState(
    existing?.icon_id ?? PLAYLIST_ICONS[0].id
  );

  // Reset on close OR re-sync when `existing` changes (reopening the modal
  // on a different playlist). See CreateLibraryModal for why the new
  // react-hooks/set-state-in-effect rule is suppressed here.
  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setName(existing?.name ?? "");
      setDescription(existing?.description ?? "");
      setSelectedColorId(existing?.color_id ?? PLAYLIST_COLORS[0].id);
      setSelectedIconId(existing?.icon_id ?? PLAYLIST_ICONS[0].id);
    }
  }, [isOpen, existing]);

  // Escape handler
  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const currentColor =
    PLAYLIST_COLORS.find((c) => c.id === selectedColorId) ?? PLAYLIST_COLORS[0];
  const currentIcon =
    PLAYLIST_ICONS.find((i) => i.id === selectedIconId) ?? PLAYLIST_ICONS[0];
  const CurrentIconComponent = currentIcon.Icon;

  const canSubmit = name.trim().length > 0;
  const displayName = name.trim() || t("playlistModal.previewDefault");

  const handleCreate = () => {
    if (!canSubmit) return;
    onCreate?.({
      name: name.trim(),
      description: description.trim(),
      colorId: selectedColorId,
      iconId: selectedIconId,
    });
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in max-h-[90vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-lg font-bold text-zinc-900 dark:text-white mb-4">
          {isEdit ? t("playlistModal.editTitle") : t("playlistModal.title")}
        </h2>

        {/* Live preview card */}
        <div
          className={`flex items-center space-x-3 p-3 rounded-xl mb-6 transition-colors duration-300 ${currentColor.previewBg}`}
        >
          <div
            className={`w-10 h-10 rounded-lg flex items-center justify-center shrink-0 transition-colors duration-300 ${currentColor.tileBg} ${currentColor.tileText}`}
          >
            <CurrentIconComponent size={20} />
          </div>
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
              {displayName}
            </div>
            <div className="text-xs text-zinc-500">
              {t("playlistModal.previewSubtitle")}
            </div>
          </div>
        </div>

        <div className="border-t border-zinc-100 dark:border-zinc-800 mb-4" />

        {/* Name field */}
        <div className="mb-4">
          <label
            htmlFor="playlist-name"
            className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2"
          >
            {t("playlistModal.nameLabel")} <span className="text-red-500">*</span>
          </label>
          <input
            id="playlist-name"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && canSubmit) handleCreate();
            }}
            placeholder={t("playlistModal.namePlaceholder")}
            autoFocus
            className="w-full px-4 py-3 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors"
          />
        </div>

        {/* Description field */}
        <div className="mb-4">
          <label
            htmlFor="playlist-description"
            className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2"
          >
            {t("playlistModal.descriptionLabel")}{" "}
            <span className="text-zinc-400 normal-case tracking-normal font-normal">
              {t("common.optional")}
            </span>
          </label>
          <textarea
            id="playlist-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder={t("playlistModal.descriptionPlaceholder")}
            rows={2}
            className="w-full px-4 py-3 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors resize-none"
          />
        </div>

        {/* Color picker */}
        <div className="mb-4">
          <div className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-3">
            {t("playlistModal.colorLabel")}
          </div>
          <div className="flex flex-wrap gap-2">
            {PLAYLIST_COLORS.map((color) => {
              const isSelected = color.id === selectedColorId;
              return (
                <button
                  key={color.id}
                  type="button"
                  onClick={() => setSelectedColorId(color.id)}
                  aria-label={t("playlistModal.colorAria", { color: color.id })}
                  aria-pressed={isSelected}
                  className={`w-8 h-8 rounded-full ${color.swatch} transition-transform hover:scale-110 ${
                    isSelected
                      ? `ring-2 ring-offset-2 ring-offset-white dark:ring-offset-surface-dark-elevated ${color.ring}`
                      : ""
                  }`}
                />
              );
            })}
          </div>
        </div>

        {/* Icon picker */}
        <div className="mb-6">
          <div className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-3">
            {t("playlistModal.iconLabel")}
          </div>
          <div className="flex flex-wrap gap-2">
            {PLAYLIST_ICONS.map((icon) => {
              const isSelected = icon.id === selectedIconId;
              const IconComponent = icon.Icon;
              return (
                <button
                  key={icon.id}
                  type="button"
                  onClick={() => setSelectedIconId(icon.id)}
                  aria-label={t("playlistModal.iconAria", { icon: icon.id })}
                  aria-pressed={isSelected}
                  className={`w-10 h-10 rounded-lg flex items-center justify-center transition-colors duration-200 ${
                    isSelected
                      ? `${currentColor.tileBg} ${currentColor.tileText}`
                      : "bg-zinc-50 dark:bg-zinc-800/50 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-600 dark:hover:text-zinc-300"
                  }`}
                >
                  <IconComponent size={18} />
                </button>
              );
            })}
          </div>
        </div>

        {/* Footer actions */}
        <div className="flex items-center justify-end space-x-3">
          <button
            type="button"
            onClick={onClose}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={handleCreate}
            disabled={!canSubmit}
            className={`px-5 py-2 rounded-xl text-sm font-semibold text-white flex items-center space-x-2 shadow-lg transition-all duration-300 active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none ${currentColor.button}`}
          >
            {isEdit ? <Check size={16} /> : <Plus size={16} />}
            <span>
              {isEdit
                ? t("playlistModal.editSubmit")
                : t("playlistModal.submit")}
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}
