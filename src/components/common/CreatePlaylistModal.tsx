import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Plus,
  Check,
  MoreHorizontal,
  Pencil,
  Image as ImageIcon,
  Trash2,
  Loader2,
} from "lucide-react";
import { PLAYLIST_COLORS, PLAYLIST_ICONS } from "../../lib/playlistVisuals";
import {
  clearPlaylistCover,
  setPlaylistCoverFromFile,
  type Playlist,
} from "../../lib/tauri/playlist";
import { pickFile } from "../../lib/tauri/dialog";
import { resolveRemoteImage } from "../../lib/tauri/artwork";
import { PlaylistIcon } from "../../lib/PlaylistIcon";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "./AnimatedModalShell";

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
  /**
   * Called whenever the cover changes from inside the modal (upload or
   * remove). The parent should refresh its playlist state so the new
   * `cover_path` flows down on the next render. Only meaningful in edit
   * mode — create mode has no cover yet.
   */
  onCoverChanged?: () => void;
}

export function CreatePlaylistModal({
  isOpen,
  onClose,
  onCreate,
  existing,
  onCoverChanged,
}: CreatePlaylistModalProps) {
  const { t } = useTranslation();
  const isEdit = existing != null;
  const [name, setName] = useState(existing?.name ?? "");
  const [description, setDescription] = useState(existing?.description ?? "");
  const [selectedColorId, setSelectedColorId] = useState(
    existing?.color_id ?? PLAYLIST_COLORS[0].id,
  );
  const [selectedIconId, setSelectedIconId] = useState(
    existing?.icon_id ?? PLAYLIST_ICONS[0].id,
  );
  // Cover-section local state. `coverMenuOpen` toggles the "..." menu
  // (Change photo / Remove photo). `coverBusy` blocks repeat clicks
  // while the upload / clear request is in flight.
  const [coverMenuOpen, setCoverMenuOpen] = useState(false);
  const [coverBusy, setCoverBusy] = useState(false);
  const coverMenuRef = useRef<HTMLDivElement>(null);

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

  // Modal a11y: Escape close, focus trap, focus restore on close.
  // Replaces the previous bespoke Escape listener.
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  // Click-outside for the cover "..." menu. Listening on `mousedown` so
  // the menu closes before the click on the trigger button can re-toggle
  // it via the bubble.
  useEffect(() => {
    if (!coverMenuOpen) return;
    const handler = (e: MouseEvent) => {
      if (
        coverMenuRef.current &&
        !coverMenuRef.current.contains(e.target as Node)
      ) {
        setCoverMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [coverMenuOpen]);

  const handlePickCover = useCallback(async () => {
    if (!existing || coverBusy) return;
    setCoverMenuOpen(false);
    const path = await pickFile(["jpg", "jpeg", "png", "webp"]);
    if (!path) return;
    setCoverBusy(true);
    try {
      await setPlaylistCoverFromFile(existing.id, path);
      onCoverChanged?.();
    } catch (err) {
      console.error("[CreatePlaylistModal] set cover failed", err);
    } finally {
      setCoverBusy(false);
    }
  }, [existing, coverBusy, onCoverChanged]);

  const handleRemoveCover = useCallback(async () => {
    if (!existing || coverBusy) return;
    setCoverMenuOpen(false);
    setCoverBusy(true);
    try {
      // Backend: clears `cover_hash` + flips `cover_is_auto` back to 1
      // + immediately re-runs the auto-cover so the user gets instant
      // visual feedback instead of an empty tile.
      await clearPlaylistCover(existing.id);
      onCoverChanged?.();
    } catch (err) {
      console.error("[CreatePlaylistModal] clear cover failed", err);
    } finally {
      setCoverBusy(false);
    }
  }, [existing, coverBusy, onCoverChanged]);

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
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="playlist-modal-title"
        className="relative w-full max-w-xl rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated max-h-[90vh] overflow-y-auto"
      >
        <h2
          id="playlist-modal-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-4"
        >
          {isEdit ? t("playlistModal.editTitle") : t("playlistModal.title")}
        </h2>

        {/* Cover editor — shown only in edit mode (a brand-new playlist
            has no id to attach an upload to). Spotify-style: large
            preview with a hover overlay (Pencil + "Choose photo") and a
            "..." menu in the top-right corner offering Change / Remove.
            The "Remove photo" path immediately re-runs the auto-cover
            backend-side so the tile stays populated rather than going
            blank between mutations. */}
        {isEdit &&
          existing &&
          (() => {
            const coverUrl = resolveRemoteImage(existing.cover_path, null);
            return (
              <div
                className={`flex items-stretch gap-4 p-3 rounded-xl mb-6 transition-colors duration-300 ${currentColor.previewBg}`}
              >
                {/* Outer container is `group` (drives the hover state) but
                  has NO `overflow-hidden` so the dropdown menu can extend
                  past the tile's right edge without being clipped. The
                  rounded corners + image clip live one level deeper. */}
                <div className="relative group w-32 h-32 shrink-0">
                  <div className="absolute inset-0 rounded-xl overflow-hidden shadow-md">
                    {coverUrl ? (
                      <img
                        src={coverUrl}
                        alt=""
                        className="w-full h-full object-cover"
                      />
                    ) : (
                      <div
                        className={`w-full h-full flex items-center justify-center ${currentColor.tileBg} ${currentColor.tileText}`}
                      >
                        <PlaylistIcon iconId={selectedIconId} size={48} />
                      </div>
                    )}
                    {/* Hover overlay — clicking anywhere on the cover opens
                      the file picker, matching Spotify's UX. */}
                    <button
                      type="button"
                      onClick={handlePickCover}
                      disabled={coverBusy}
                      aria-label={t(
                        "playlistModal.coverChoose",
                        "Choose photo",
                      )}
                      className="absolute inset-0 bg-black/60 text-white opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity flex flex-col items-center justify-center gap-1 disabled:cursor-not-allowed"
                    >
                      {coverBusy ? (
                        <Loader2 size={28} className="animate-spin" />
                      ) : (
                        <>
                          <Pencil size={24} />
                          <span className="text-xs font-medium">
                            {t("playlistModal.coverChoose", "Choose photo")}
                          </span>
                        </>
                      )}
                    </button>
                  </div>
                  {/* "..." menu sits at the OUTER level (sibling of the
                    clipping container) so its dropdown can spill out to
                    the right of the cover tile without being cropped.
                    Stops propagation so the click doesn't also fire the
                    cover-area picker underneath. */}
                  <div
                    ref={coverMenuRef}
                    className="absolute top-1.5 right-1.5 z-30"
                  >
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        setCoverMenuOpen((v) => !v);
                      }}
                      aria-label={t("playlistModal.coverMenu", "Cover options")}
                      className="w-7 h-7 rounded-full bg-black/60 text-white opacity-0 group-hover:opacity-100 focus:opacity-100 transition-opacity flex items-center justify-center hover:bg-black/80"
                    >
                      <MoreHorizontal size={16} />
                    </button>
                    {coverMenuOpen && (
                      <div
                        // Anchor to the trigger's right edge (`left-full`)
                        // with a small gap (`ml-1`) so the dropdown opens
                        // INTO the modal toward the form fields, not back
                        // out the left side past the cover (which would
                        // clip on the modal's edge).
                        className="absolute left-full top-0 ml-1 w-44 rounded-lg shadow-2xl border bg-white text-zinc-800 border-zinc-200 dark:bg-zinc-800 dark:text-zinc-100 dark:border-zinc-700 py-1 text-sm"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          onClick={handlePickCover}
                          disabled={coverBusy}
                          className="w-full px-3 py-2 flex items-center gap-2 hover:bg-zinc-100 dark:hover:bg-zinc-700 transition-colors disabled:opacity-50"
                        >
                          <ImageIcon size={14} />
                          <span>
                            {t("playlistModal.coverChange", "Change photo")}
                          </span>
                        </button>
                        {existing.cover_hash && (
                          <button
                            type="button"
                            onClick={handleRemoveCover}
                            disabled={coverBusy}
                            className="w-full px-3 py-2 flex items-center gap-2 hover:bg-zinc-100 dark:hover:bg-zinc-700 transition-colors disabled:opacity-50"
                          >
                            <Trash2 size={14} />
                            <span>
                              {t("playlistModal.coverRemove", "Remove photo")}
                            </span>
                          </button>
                        )}
                      </div>
                    )}
                  </div>
                </div>
                <div className="flex-1 min-w-0 flex flex-col justify-center">
                  <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
                    {displayName}
                  </div>
                  <div className="text-xs text-zinc-500 mt-1">
                    {existing.cover_is_auto === 1
                      ? t(
                          "playlistModal.coverAutoHint",
                          "Cover automatique — se met à jour avec le contenu",
                        )
                      : t(
                          "playlistModal.coverManualHint",
                          "Image personnalisée",
                        )}
                  </div>
                </div>
              </div>
            );
          })()}

        {/* Live preview card (shown in create mode only — edit mode uses
            the cover editor block above). */}
        {!isEdit && (
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
        )}

        <div className="border-t border-zinc-100 dark:border-zinc-800 mb-4" />

        {/* Name field */}
        <div className="mb-4">
          <label
            htmlFor="playlist-name"
            className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2"
          >
            {t("playlistModal.nameLabel")}{" "}
            <span className="text-red-500">*</span>
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
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
