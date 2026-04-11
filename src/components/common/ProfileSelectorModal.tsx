import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { X, Plus, ArrowLeft } from "lucide-react";

interface ProfileSelectorModalProps {
  isOpen: boolean;
  onClose: () => void;
}

type ProfileModalView = "select" | "create";

interface ProfileColor {
  id: string;
  swatch: string;
  ring: string;
  glow: string;
  iconBorder: string;
  iconText: string;
  button: string;
}

const PROFILE_COLORS: ProfileColor[] = [
  {
    id: "emerald",
    swatch: "bg-emerald-500",
    ring: "ring-emerald-400",
    glow: "bg-emerald-500/25",
    iconBorder: "border-emerald-500/40",
    iconText: "text-emerald-400",
    button: "bg-emerald-500 hover:bg-emerald-400 shadow-emerald-500/20",
  },
  {
    id: "violet",
    swatch: "bg-violet-500",
    ring: "ring-violet-400",
    glow: "bg-violet-500/25",
    iconBorder: "border-violet-500/40",
    iconText: "text-violet-400",
    button: "bg-violet-500 hover:bg-violet-400 shadow-violet-500/20",
  },
  {
    id: "sky",
    swatch: "bg-sky-500",
    ring: "ring-sky-400",
    glow: "bg-sky-500/25",
    iconBorder: "border-sky-500/40",
    iconText: "text-sky-400",
    button: "bg-sky-500 hover:bg-sky-400 shadow-sky-500/20",
  },
  {
    id: "amber",
    swatch: "bg-amber-500",
    ring: "ring-amber-400",
    glow: "bg-amber-500/25",
    iconBorder: "border-amber-500/40",
    iconText: "text-amber-400",
    button: "bg-amber-500 hover:bg-amber-400 shadow-amber-500/20",
  },
  {
    id: "red",
    swatch: "bg-red-500",
    ring: "ring-red-400",
    glow: "bg-red-500/25",
    iconBorder: "border-red-500/40",
    iconText: "text-red-400",
    button: "bg-red-500 hover:bg-red-400 shadow-red-500/20",
  },
  {
    id: "indigo",
    swatch: "bg-indigo-500",
    ring: "ring-indigo-400",
    glow: "bg-indigo-500/25",
    iconBorder: "border-indigo-500/40",
    iconText: "text-indigo-400",
    button: "bg-indigo-500 hover:bg-indigo-400 shadow-indigo-500/20",
  },
  {
    id: "lime",
    swatch: "bg-lime-500",
    ring: "ring-lime-400",
    glow: "bg-lime-500/25",
    iconBorder: "border-lime-500/40",
    iconText: "text-lime-400",
    button: "bg-lime-500 hover:bg-lime-400 shadow-lime-500/20",
  },
  {
    id: "orange",
    swatch: "bg-orange-500",
    ring: "ring-orange-400",
    glow: "bg-orange-500/25",
    iconBorder: "border-orange-500/40",
    iconText: "text-orange-400",
    button: "bg-orange-500 hover:bg-orange-400 shadow-orange-500/20",
  },
  {
    id: "rose",
    swatch: "bg-rose-500",
    ring: "ring-rose-400",
    glow: "bg-rose-500/25",
    iconBorder: "border-rose-500/40",
    iconText: "text-rose-400",
    button: "bg-rose-500 hover:bg-rose-400 shadow-rose-500/20",
  },
  {
    id: "teal",
    swatch: "bg-teal-500",
    ring: "ring-teal-400",
    glow: "bg-teal-500/25",
    iconBorder: "border-teal-500/40",
    iconText: "text-teal-400",
    button: "bg-teal-500 hover:bg-teal-400 shadow-teal-500/20",
  },
];

export function ProfileSelectorModal({
  isOpen,
  onClose,
}: ProfileSelectorModalProps) {
  const { t } = useTranslation();
  const [view, setView] = useState<ProfileModalView>("select");
  const [newProfileName, setNewProfileName] = useState("");
  const [selectedColorId, setSelectedColorId] = useState(PROFILE_COLORS[0].id);

  // Reset internal state when the modal closes
  useEffect(() => {
    if (!isOpen) {
      setView("select");
      setNewProfileName("");
      setSelectedColorId(PROFILE_COLORS[0].id);
    }
  }, [isOpen]);

  // Escape handling: if we're on "create", step back to "select" first
  useEffect(() => {
    if (!isOpen) return;
    const handleKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (view === "create") {
        setView("select");
      } else {
        onClose();
      }
    };
    document.addEventListener("keydown", handleKey);
    return () => document.removeEventListener("keydown", handleKey);
  }, [isOpen, onClose, view]);

  if (!isOpen) return null;

  const canSubmit = newProfileName.trim().length > 0;
  const currentColor =
    PROFILE_COLORS.find((c) => c.id === selectedColorId) ?? PROFILE_COLORS[0];

  const handleCreateProfile = () => {
    if (!canSubmit) return;
    // TODO: persist the profile once the backend layer lands
    onClose();
  };

  return (
    <div
      className="fixed inset-0 z-100 bg-black/80 flex items-center justify-center animate-fade-in p-4"
      onClick={onClose}
    >
      {view === "select" && (
        <>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.close")}
            className="absolute top-6 right-6 p-2 rounded-full text-zinc-400 hover:text-white hover:bg-zinc-800 transition-colors"
          >
            <X size={24} />
          </button>

          <div className="text-center" onClick={(e) => e.stopPropagation()}>
            <h2 className="text-4xl font-bold text-white mb-3">
              {t("profiles.select.title")}
            </h2>
            <p className="text-zinc-500 mb-12">
              {t("profiles.select.subtitle")}
            </p>

            <div className="flex items-center justify-center space-x-8">
              {/* Default Profile */}
              <button
                type="button"
                onClick={onClose}
                className="group flex flex-col items-center space-y-3"
              >
                <div className="w-32 h-32 rounded-2xl bg-zinc-800 border-2 border-emerald-500 flex items-center justify-center text-5xl font-bold text-zinc-400 group-hover:border-emerald-400 transition-colors shadow-lg shadow-emerald-500/20">
                  D
                </div>
                <span className="text-white font-medium">Default</span>
                <span className="text-xs text-zinc-500">
                  {t("profiles.select.edit")}
                </span>
              </button>

              {/* Add Profile */}
              <button
                type="button"
                onClick={() => setView("create")}
                className="group flex flex-col items-center space-y-3"
              >
                <div className="w-32 h-32 rounded-2xl border-2 border-dashed border-zinc-600 flex items-center justify-center text-zinc-500 group-hover:border-zinc-500 group-hover:text-zinc-400 transition-colors">
                  <Plus size={40} />
                </div>
                <span className="text-zinc-500 font-medium">
                  {t("profiles.select.add")}
                </span>
              </button>
            </div>
          </div>
        </>
      )}

      {view === "create" && (
        <div
          className="relative w-full max-w-md rounded-3xl border border-zinc-800 bg-surface-dark-elevated p-8 shadow-2xl overflow-hidden animate-fade-in"
          onClick={(e) => e.stopPropagation()}
        >
          {/* Back button */}
          <button
            type="button"
            onClick={() => setView("select")}
            className="flex items-center space-x-1 text-sm text-zinc-400 hover:text-zinc-200 transition-colors mb-6"
          >
            <ArrowLeft size={16} />
            <span>{t("common.back")}</span>
          </button>

          {/* Icon with breathing glow — driven by the currently selected color */}
          <div className="flex flex-col items-center text-center mb-6">
            <div className="relative mb-4">
              <div
                aria-hidden="true"
                className={`pointer-events-none absolute inset-0 -m-10 rounded-full blur-3xl animate-breathing transition-colors duration-300 ${currentColor.glow}`}
              />
              <div
                className={`relative w-20 h-20 rounded-2xl bg-zinc-800 border flex items-center justify-center shadow-sm transition-colors duration-300 ${currentColor.iconBorder} ${currentColor.iconText}`}
              >
                <span className="text-5xl font-bold leading-none">?</span>
              </div>
            </div>
            <h2 className="text-2xl font-bold text-white mb-2">
              {t("profiles.create.title")}
            </h2>
            <p className="text-sm text-zinc-500">
              {t("profiles.create.subtitle")}
            </p>
          </div>

          <div className="border-t border-zinc-800 mb-6" />

          {/* Name field */}
          <div className="mb-6">
            <label
              htmlFor="profile-name"
              className="block text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-2"
            >
              {t("profiles.create.nameLabel")}
            </label>
            <input
              id="profile-name"
              type="text"
              value={newProfileName}
              onChange={(e) => setNewProfileName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && canSubmit) handleCreateProfile();
              }}
              placeholder={t("profiles.create.namePlaceholder")}
              autoFocus
              className="w-full px-4 py-3 rounded-xl bg-zinc-800/50 border border-zinc-700 text-white placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors"
            />
          </div>

          {/* Color picker */}
          <div className="mb-8">
            <div className="block text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-3">
              {t("profiles.create.colorLabel")}
            </div>
            <div className="flex flex-wrap gap-2">
              {PROFILE_COLORS.map((color) => {
                const isSelected = color.id === selectedColorId;
                return (
                  <button
                    key={color.id}
                    type="button"
                    onClick={() => setSelectedColorId(color.id)}
                    aria-label={t("profiles.create.colorAria", { color: color.id })}
                    aria-pressed={isSelected}
                    className={`w-8 h-8 rounded-full ${color.swatch} transition-transform hover:scale-110 ${
                      isSelected
                        ? `ring-2 ring-offset-2 ring-offset-zinc-900 ${color.ring}`
                        : ""
                    }`}
                  />
                );
              })}
            </div>
          </div>

          {/* Footer actions */}
          <div className="flex items-center justify-end space-x-3">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-400 hover:text-zinc-200 transition-colors"
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              onClick={handleCreateProfile}
              disabled={!canSubmit}
              className={`px-5 py-2 rounded-xl text-sm font-semibold text-white flex items-center space-x-2 shadow-lg transition-all duration-300 active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none ${currentColor.button}`}
            >
              <Plus size={16} />
              <span>{t("profiles.create.submit")}</span>
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
