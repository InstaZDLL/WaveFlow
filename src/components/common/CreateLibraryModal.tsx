import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Library, Plus } from "lucide-react";

interface CreateLibraryModalProps {
  isOpen: boolean;
  onClose: () => void;
  onCreate?: (name: string, description: string) => void;
}

export function CreateLibraryModal({
  isOpen,
  onClose,
  onCreate,
}: CreateLibraryModalProps) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");

  // Reset when the modal closes
  useEffect(() => {
    if (!isOpen) {
      setName("");
      setDescription("");
    }
  }, [isOpen]);

  // Close on Escape
  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const canSubmit = name.trim().length > 0;
  const displayName = name.trim() || t("libraryModal.previewDefault");

  const handleCreate = () => {
    if (!canSubmit) return;
    onCreate?.(name.trim(), description.trim());
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      <div
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-lg font-bold text-zinc-900 dark:text-white mb-4">
          {t("libraryModal.title")}
        </h2>

        {/* Live preview card */}
        <div className="flex items-center space-x-3 p-3 rounded-xl bg-emerald-50 dark:bg-emerald-900/20 mb-6">
          <div className="w-10 h-10 rounded-lg bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400 flex items-center justify-center shrink-0">
            <Library size={20} />
          </div>
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-zinc-800 dark:text-zinc-200 truncate">
              {displayName}
            </div>
            <div className="text-xs text-zinc-500">
              {t("libraryModal.previewSubtitle")}
            </div>
          </div>
        </div>

        <div className="border-t border-zinc-100 dark:border-zinc-800 mb-4" />

        {/* Name field */}
        <div className="mb-4">
          <label
            htmlFor="library-name"
            className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2"
          >
            {t("libraryModal.nameLabel")} <span className="text-red-500">*</span>
          </label>
          <input
            id="library-name"
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && canSubmit) handleCreate();
            }}
            placeholder={t("libraryModal.namePlaceholder")}
            autoFocus
            className="w-full px-4 py-3 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors"
          />
        </div>

        {/* Description field */}
        <div className="mb-6">
          <label
            htmlFor="library-description"
            className="block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2"
          >
            {t("libraryModal.descriptionLabel")}{" "}
            <span className="text-zinc-400 normal-case tracking-normal font-normal">
              {t("common.optional")}
            </span>
          </label>
          <textarea
            id="library-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder={t("libraryModal.descriptionPlaceholder")}
            rows={3}
            className="w-full px-4 py-3 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors resize-none"
          />
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
            className="px-5 py-2 rounded-xl text-sm font-semibold bg-emerald-500 hover:bg-emerald-400 text-white flex items-center space-x-2 shadow-lg shadow-emerald-500/20 transition-all active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none"
          >
            <Plus size={16} />
            <span>{t("libraryModal.submit")}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
