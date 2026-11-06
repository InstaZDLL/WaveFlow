import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ImagePlus, Trash2 } from "lucide-react";
import { useProfile } from "../../hooks/useProfile";
import { pickFile } from "../../lib/tauri/dialog";
import {
  avatarRefusalKey,
  PROFILE_PICTURE_EXTENSIONS,
  type Profile,
} from "../../lib/tauri/profile";
import { ProfileAvatar } from "./ProfileAvatar";

/**
 * Choose, change or remove a profile's picture: the picture (or the
 * initial) beside two buttons, and the reason under them when a file was
 * refused. Shared by Settings → General → Profile and the onboarding's
 * picture screen, so both say the same thing about a refused file.
 *
 * `variant="stage"` is the onboarding's: a large picture above centred
 * buttons. `onChanged` runs after a picture was set or removed.
 */
export function ProfilePictureEditor({
  profile,
  variant = "row",
  onChanged,
}: {
  profile: Profile | null;
  variant?: "row" | "stage";
  onChanged?: () => void;
}) {
  const { t } = useTranslation();
  const { setProfileAvatar, clearProfileAvatar } = useProfile();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const hasPicture = profile?.avatar_path != null;

  const choose = async () => {
    if (!profile || busy) return;
    // The profile the click was made for, whatever is active once the
    // dialog closes.
    const profileId = profile.id;
    const path = await pickFile(
      PROFILE_PICTURE_EXTENSIONS,
      t("profiles.picture.pickerTitle"),
    );
    if (!path) return;
    setBusy(true);
    try {
      await setProfileAvatar(profileId, path);
      setError(null);
      onChanged?.();
    } catch (err) {
      console.error("[ProfilePicture] set failed", err);
      setError(t(avatarRefusalKey(err) ?? "profiles.picture.errors.failed"));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!profile || busy) return;
    setBusy(true);
    try {
      await clearProfileAvatar(profile.id);
      setError(null);
      onChanged?.();
    } catch (err) {
      console.error("[ProfilePicture] clear failed", err);
      setError(t("profiles.picture.errors.removeFailed"));
    } finally {
      setBusy(false);
    }
  };

  const stage = variant === "stage";
  const buttons = (
    <div
      className={`flex flex-wrap items-center gap-2 ${stage ? "justify-center" : ""}`}
    >
      <button
        type="button"
        onClick={() => void choose()}
        disabled={!profile || busy}
        data-profile-picture-choose
        className="inline-flex items-center gap-1.5 rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 disabled:cursor-not-allowed disabled:opacity-50 dark:border-zinc-700 dark:text-zinc-200"
      >
        <ImagePlus size={14} aria-hidden="true" />
        {hasPicture
          ? t("profiles.picture.change")
          : t("profiles.picture.choose")}
      </button>
      {hasPicture && (
        <button
          type="button"
          onClick={() => void remove()}
          disabled={busy}
          data-profile-picture-remove
          className="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-xs font-medium text-zinc-500 transition-colors hover:text-rose-600 disabled:cursor-not-allowed disabled:opacity-50 dark:text-zinc-400 dark:hover:text-rose-400"
        >
          <Trash2 size={14} aria-hidden="true" />
          {t("profiles.picture.remove")}
        </button>
      )}
    </div>
  );
  const message = error && (
    <p
      role="alert"
      className={`text-xs text-rose-500 ${stage ? "text-center" : ""}`}
    >
      {error}
    </p>
  );

  if (stage) {
    return (
      <div className="flex flex-col items-center gap-4">
        <ProfileAvatar
          profile={profile}
          className="h-28 w-28 rounded-full text-5xl font-bold shadow-lg"
        />
        {buttons}
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("profiles.picture.formats")}
        </p>
        {message}
      </div>
    );
  }
  return (
    <div className="flex items-center gap-4">
      <ProfileAvatar
        profile={profile}
        className="h-14 w-14 rounded-full text-2xl font-bold"
      />
      <div className="min-w-0 flex-1 space-y-1.5">
        {buttons}
        {message}
      </div>
    </div>
  );
}
