import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Music2,
  Heart,
  Star,
  Flame,
  Moon,
  Sun,
  Cloud,
  Coffee,
  Leaf,
  Gift,
  Headphones,
  Plus,
  type LucideIcon,
} from "lucide-react";

interface CreatePlaylistModalProps {
  isOpen: boolean;
  onClose: () => void;
  onCreate?: (data: {
    name: string;
    description: string;
    colorId: string;
    iconId: string;
  }) => void;
}

interface PlaylistColor {
  id: string;
  swatch: string;
  ring: string;
  tileBg: string;
  tileText: string;
  previewBg: string;
  button: string;
}

const PLAYLIST_COLORS: PlaylistColor[] = [
  {
    id: "violet",
    swatch: "bg-violet-500",
    ring: "ring-violet-400",
    tileBg: "bg-violet-100 dark:bg-violet-950/60",
    tileText: "text-violet-500 dark:text-violet-400",
    previewBg: "bg-violet-50 dark:bg-violet-900/20",
    button: "bg-violet-500 hover:bg-violet-400 shadow-violet-500/20",
  },
  {
    id: "emerald",
    swatch: "bg-emerald-500",
    ring: "ring-emerald-400",
    tileBg: "bg-emerald-100 dark:bg-emerald-950/60",
    tileText: "text-emerald-500 dark:text-emerald-400",
    previewBg: "bg-emerald-50 dark:bg-emerald-900/20",
    button: "bg-emerald-500 hover:bg-emerald-400 shadow-emerald-500/20",
  },
  {
    id: "sky",
    swatch: "bg-sky-500",
    ring: "ring-sky-400",
    tileBg: "bg-sky-100 dark:bg-sky-950/60",
    tileText: "text-sky-500 dark:text-sky-400",
    previewBg: "bg-sky-50 dark:bg-sky-900/20",
    button: "bg-sky-500 hover:bg-sky-400 shadow-sky-500/20",
  },
  {
    id: "amber",
    swatch: "bg-amber-500",
    ring: "ring-amber-400",
    tileBg: "bg-amber-100 dark:bg-amber-950/60",
    tileText: "text-amber-500 dark:text-amber-400",
    previewBg: "bg-amber-50 dark:bg-amber-900/20",
    button: "bg-amber-500 hover:bg-amber-400 shadow-amber-500/20",
  },
  {
    id: "rose",
    swatch: "bg-rose-500",
    ring: "ring-rose-400",
    tileBg: "bg-rose-100 dark:bg-rose-950/60",
    tileText: "text-rose-500 dark:text-rose-400",
    previewBg: "bg-rose-50 dark:bg-rose-900/20",
    button: "bg-rose-500 hover:bg-rose-400 shadow-rose-500/20",
  },
  {
    id: "purple",
    swatch: "bg-purple-500",
    ring: "ring-purple-400",
    tileBg: "bg-purple-100 dark:bg-purple-950/60",
    tileText: "text-purple-500 dark:text-purple-400",
    previewBg: "bg-purple-50 dark:bg-purple-900/20",
    button: "bg-purple-500 hover:bg-purple-400 shadow-purple-500/20",
  },
  {
    id: "pink",
    swatch: "bg-pink-500",
    ring: "ring-pink-400",
    tileBg: "bg-pink-100 dark:bg-pink-950/60",
    tileText: "text-pink-500 dark:text-pink-400",
    previewBg: "bg-pink-50 dark:bg-pink-900/20",
    button: "bg-pink-500 hover:bg-pink-400 shadow-pink-500/20",
  },
  {
    id: "teal",
    swatch: "bg-teal-500",
    ring: "ring-teal-400",
    tileBg: "bg-teal-100 dark:bg-teal-950/60",
    tileText: "text-teal-500 dark:text-teal-400",
    previewBg: "bg-teal-50 dark:bg-teal-900/20",
    button: "bg-teal-500 hover:bg-teal-400 shadow-teal-500/20",
  },
  {
    id: "orange",
    swatch: "bg-orange-500",
    ring: "ring-orange-400",
    tileBg: "bg-orange-100 dark:bg-orange-950/60",
    tileText: "text-orange-500 dark:text-orange-400",
    previewBg: "bg-orange-50 dark:bg-orange-900/20",
    button: "bg-orange-500 hover:bg-orange-400 shadow-orange-500/20",
  },
  {
    id: "lime",
    swatch: "bg-lime-500",
    ring: "ring-lime-400",
    tileBg: "bg-lime-100 dark:bg-lime-950/60",
    tileText: "text-lime-500 dark:text-lime-400",
    previewBg: "bg-lime-50 dark:bg-lime-900/20",
    button: "bg-lime-500 hover:bg-lime-400 shadow-lime-500/20",
  },
];

interface PlaylistIconEntry {
  id: string;
  Icon: LucideIcon;
}

const PLAYLIST_ICONS: PlaylistIconEntry[] = [
  { id: "music", Icon: Music2 },
  { id: "heart", Icon: Heart },
  { id: "star", Icon: Star },
  { id: "flame", Icon: Flame },
  { id: "moon", Icon: Moon },
  { id: "sun", Icon: Sun },
  { id: "cloud", Icon: Cloud },
  { id: "coffee", Icon: Coffee },
  { id: "leaf", Icon: Leaf },
  { id: "gift", Icon: Gift },
  { id: "headphones", Icon: Headphones },
];

export function CreatePlaylistModal({
  isOpen,
  onClose,
  onCreate,
}: CreatePlaylistModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [selectedColorId, setSelectedColorId] = useState(PLAYLIST_COLORS[0].id);
  const [selectedIconId, setSelectedIconId] = useState(PLAYLIST_ICONS[0].id);

  // Reset on close
  useEffect(() => {
    if (!isOpen) {
      setName("");
      setDescription("");
      setSelectedColorId(PLAYLIST_COLORS[0].id);
      setSelectedIconId(PLAYLIST_ICONS[0].id);
    }
  }, [isOpen]);

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
          {t("playlistModal.title")}
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
            <Plus size={16} />
            <span>{t("playlistModal.submit")}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
