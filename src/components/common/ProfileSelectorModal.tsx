import { useEffect, useState, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import { X, Plus, ArrowLeft, Check, Pencil, Trash2 } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useProfile } from "../../hooks/useProfile";
import type { Profile } from "../../lib/tauri/profile";
import {
  PROFILE_COLORS,
  DEFAULT_PROFILE_COLOR_ID,
  getProfileColor,
  profileInitial,
} from "../../lib/profileColors";

interface ProfileSelectorModalProps {
  isOpen: boolean;
  onClose: () => void;
}

type ProfileModalView = "select" | "create" | "delete";

export function ProfileSelectorModal({
  isOpen,
  onClose,
}: ProfileSelectorModalProps) {
  const { t } = useTranslation();
  const {
    profiles,
    activeProfile,
    createProfile,
    switchProfile,
    deleteProfile,
  } = useProfile();

  const [view, setView] = useState<ProfileModalView>("select");
  const [newProfileName, setNewProfileName] = useState("");
  const [selectedColorId, setSelectedColorId] = useState(
    DEFAULT_PROFILE_COLOR_ID,
  );
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  const [isManaging, setIsManaging] = useState(false);
  const [profileToDelete, setProfileToDelete] = useState<Profile | null>(null);

  // Reset internal state when the modal closes
  useEffect(() => {
    if (!isOpen) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setView("select");
      setNewProfileName("");
      setSelectedColorId(DEFAULT_PROFILE_COLOR_ID);
      setIsSubmitting(false);
      setSubmitError(null);
      setIsManaging(false);
      setProfileToDelete(null);
    }
  }, [isOpen]);

  // Auto-exit management mode once only the active profile remains — the
  // active one can't be deleted from here, so there's nothing left to manage.
  const canManage = profiles.length > 1;
  const showManageControls = isManaging && canManage;

  // Escape handling: if we're on "create" or "delete", step back to "select"
  // first instead of dismissing the whole modal — the user might just want
  // to abort, not lose their place. useModalA11y dispatches Escape here.
  const handleEscape = useCallback(() => {
    if (view === "create" || view === "delete") {
      setView("select");
      setProfileToDelete(null);
      setSubmitError(null);
    } else {
      onClose();
    }
  }, [view, onClose]);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, handleEscape);

  const canSubmit = newProfileName.trim().length > 0 && !isSubmitting;
  const currentColor = getProfileColor(selectedColorId);

  const handleSelectProfile = async (profile: Profile) => {
    if (profile.id === activeProfile?.id) {
      onClose();
      return;
    }
    try {
      await switchProfile(profile.id);
      onClose();
    } catch (err) {
      console.error("[ProfileSelectorModal] switch failed", err);
    }
  };

  const handleRequestDelete = (profile: Profile) => {
    setProfileToDelete(profile);
    setSubmitError(null);
    setView("delete");
  };

  const handleConfirmDelete = async () => {
    if (!profileToDelete || isSubmitting) return;
    setIsSubmitting(true);
    setSubmitError(null);
    try {
      await deleteProfile(profileToDelete.id);
      setProfileToDelete(null);
      setView("select");
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setSubmitError(message);
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleCreateProfile = async () => {
    if (!canSubmit) return;
    setIsSubmitting(true);
    setSubmitError(null);
    try {
      const created = await createProfile({
        name: newProfileName.trim(),
        color_id: selectedColorId,
      });
      // Auto-activate the freshly created profile so the user lands directly
      // in their new environment — matches what most profile managers do.
      await switchProfile(created.id);
      onClose();
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setSubmitError(message);
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <AnimatePresence>
      {isOpen && (
        <motion.div
          ref={dialogRef}
          role="dialog"
          aria-modal="true"
          aria-label={
            view === "create"
              ? t("profiles.create.title")
              : view === "delete"
                ? t("profiles.delete.title", {
                    name: profileToDelete?.name ?? "",
                  })
                : t("profiles.select.title")
          }
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.18, ease: "easeOut" }}
          className="fixed inset-0 z-100 bg-black/80 backdrop-blur-md flex items-center justify-center p-4"
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

          {/* Manage toggle — Netflix-style "Manage profiles" affordance.
              Only shown when at least one deletable profile exists. */}
          {canManage && (
            <button
              type="button"
              onClick={(e) => {
                e.stopPropagation();
                setIsManaging((v) => !v);
              }}
              className="absolute top-6 left-6 inline-flex items-center gap-2 px-3 py-2 rounded-full text-sm text-zinc-400 hover:text-white hover:bg-zinc-800 transition-colors"
            >
              {showManageControls ? <Check size={16} /> : <Pencil size={16} />}
              <span>
                {showManageControls
                  ? t("profiles.manage.done")
                  : t("profiles.manage.toggle")}
              </span>
            </button>
          )}

          <div className="text-center" onClick={(e) => e.stopPropagation()}>
            <h2 className="text-4xl font-bold text-white mb-3">
              {showManageControls
                ? t("profiles.manage.title")
                : t("profiles.select.title")}
            </h2>
            <p className="text-zinc-500 mb-12">
              {showManageControls
                ? t("profiles.manage.subtitle")
                : t("profiles.select.subtitle")}
            </p>

            <div className="flex flex-wrap items-start justify-center gap-8 max-w-3xl">
              {profiles.map((profile) => {
                const color = getProfileColor(profile.color_id);
                const isActive = profile.id === activeProfile?.id;
                const canDelete = showManageControls && !isActive;
                return (
                  <div
                    key={profile.id}
                    className="group flex flex-col items-center space-y-3 relative"
                  >
                    <button
                      type="button"
                      onClick={() => handleSelectProfile(profile)}
                      disabled={showManageControls && isActive}
                      className="flex flex-col items-center space-y-3 disabled:cursor-not-allowed"
                    >
                      <div
                        className={`relative w-32 h-32 rounded-2xl bg-zinc-800 border-2 flex items-center justify-center text-5xl font-bold text-zinc-300 transition-colors shadow-lg ${
                          isActive
                            ? `${color.iconBorder.replace("/40", "")} shadow-black/40`
                            : "border-zinc-700 group-hover:border-zinc-500"
                        } ${showManageControls && isActive ? "opacity-60" : ""}`}
                      >
                        {profileInitial(profile.name)}
                        {isActive && !showManageControls && (
                          <div
                            className={`absolute -bottom-2 -right-2 w-8 h-8 rounded-full ${color.avatarBg} text-white flex items-center justify-center shadow-lg ring-4 ring-zinc-900`}
                          >
                            <Check size={16} strokeWidth={3} />
                          </div>
                        )}
                      </div>
                      <span className="text-white font-medium">
                        {profile.name}
                      </span>
                      <span className="text-xs text-zinc-500">
                        {showManageControls
                          ? isActive
                            ? t("profiles.manage.activeLocked")
                            : t("profiles.manage.deleteHint")
                          : isActive
                            ? t("profiles.select.active")
                            : t("profiles.select.switchTo")}
                      </span>
                    </button>
                    {canDelete && (
                      <button
                        type="button"
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRequestDelete(profile);
                        }}
                        aria-label={t("profiles.manage.deleteAria", {
                          name: profile.name,
                        })}
                        className="absolute top-0 right-0 -translate-y-1/3 translate-x-1/3 w-9 h-9 rounded-full bg-red-500/90 text-white flex items-center justify-center shadow-lg ring-4 ring-zinc-900 hover:bg-red-500 transition-colors"
                      >
                        <Trash2 size={16} />
                      </button>
                    )}
                  </div>
                );
              })}

              {/* Add Profile (hidden in manage mode to keep the focus on deletion) */}
              {!showManageControls && (
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
              )}
            </div>
          </div>
        </>
      )}

      {view === "delete" && profileToDelete && (
        <div
          className="relative w-full max-w-md rounded-3xl border border-zinc-800 bg-surface-dark-elevated p-8 shadow-2xl overflow-hidden"
          onClick={(e) => e.stopPropagation()}
        >
          <button
            type="button"
            onClick={() => {
              setView("select");
              setProfileToDelete(null);
              setSubmitError(null);
            }}
            disabled={isSubmitting}
            className="flex items-center space-x-1 text-sm text-zinc-400 hover:text-zinc-200 transition-colors mb-6 disabled:opacity-50"
          >
            <ArrowLeft size={16} />
            <span>{t("common.back")}</span>
          </button>

          <div className="flex flex-col items-center text-center mb-6">
            <div className="relative mb-4">
              <div
                aria-hidden="true"
                className="pointer-events-none absolute inset-0 -m-10 rounded-full blur-3xl animate-breathing transition-colors duration-300 bg-red-500/30"
              />
              <div className="relative w-20 h-20 rounded-2xl bg-zinc-800 border border-red-500/40 text-red-400 flex items-center justify-center shadow-sm">
                <Trash2 size={32} />
              </div>
            </div>
            <h2 className="text-2xl font-bold text-white mb-2">
              {t("profiles.delete.title", { name: profileToDelete.name })}
            </h2>
            <p className="text-sm text-zinc-400 leading-relaxed">
              {t("profiles.delete.subtitle")}
            </p>
          </div>

          <div className="border-t border-zinc-800 mb-6" />

          <div className="mb-6 px-4 py-3 rounded-xl bg-red-500/10 border border-red-500/30 text-sm text-red-300 leading-relaxed">
            {t("profiles.delete.warning")}
          </div>

          {submitError && (
            <div className="mb-4 px-4 py-3 rounded-xl bg-red-500/10 border border-red-500/40 text-sm text-red-400">
              {submitError}
            </div>
          )}

          <div className="flex items-center justify-end space-x-3">
            <button
              type="button"
              onClick={() => {
                setView("select");
                setProfileToDelete(null);
                setSubmitError(null);
              }}
              disabled={isSubmitting}
              className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-400 hover:text-zinc-200 transition-colors disabled:opacity-50"
            >
              {t("common.cancel")}
            </button>
            <button
              type="button"
              onClick={handleConfirmDelete}
              disabled={isSubmitting}
              className="px-5 py-2 rounded-xl text-sm font-semibold text-white flex items-center space-x-2 shadow-lg transition-all duration-300 active:scale-[0.98] bg-red-500 hover:bg-red-600 disabled:opacity-50 disabled:cursor-not-allowed disabled:pointer-events-none"
            >
              <Trash2 size={16} />
              <span>
                {isSubmitting
                  ? t("profiles.delete.submitting")
                  : t("profiles.delete.submit")}
              </span>
            </button>
          </div>
        </div>
      )}

      {view === "create" && (
        <div
          className="relative w-full max-w-md rounded-3xl border border-zinc-800 bg-surface-dark-elevated p-8 shadow-2xl overflow-hidden"
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
                <span className="text-5xl font-bold leading-none">
                  {newProfileName.trim() ? profileInitial(newProfileName) : "?"}
                </span>
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
                if (e.key === "Enter" && canSubmit) {
                  void handleCreateProfile();
                }
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
                    aria-label={t("profiles.create.colorAria", {
                      color: color.id,
                    })}
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

          {submitError && (
            <div className="mb-4 px-4 py-3 rounded-xl bg-red-500/10 border border-red-500/40 text-sm text-red-400">
              {submitError}
            </div>
          )}

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
        </motion.div>
      )}
    </AnimatePresence>
  );
}
