import { useTranslation } from "react-i18next";
import { useProfile } from "../../../hooks/useProfile";
import { ProfilePictureEditor } from "../../common/ProfilePictureEditor";
import { StepHeading } from "./StepHeading";

/**
 * An optional picture for the profile, from a file on the disk. The
 * screen's primary button reads "Skip" until one is set; Settings →
 * General → Profile changes or removes it later.
 */
export function PhotoStep({ onTouch }: { onTouch: () => void }) {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  return (
    <div className="mx-auto max-w-xl py-4">
      <StepHeading
        title={t("onboarding.photo.title")}
        lede={t("onboarding.photo.lede")}
      />
      <div className="onboarding-tile mt-8 rounded-2xl border border-zinc-200 px-6 py-8 dark:border-zinc-800">
        <ProfilePictureEditor
          profile={activeProfile}
          variant="stage"
          onChanged={onTouch}
        />
      </div>
    </div>
  );
}
