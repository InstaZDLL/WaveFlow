import { useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertCircle, Globe, UserRound } from "lucide-react";
import { normalizeSupportedLanguageCode } from "../../../i18n";
import { LanguageDropdown } from "../../views/settings/LanguageDropdown";
import { WaveFlowLogo } from "../../common/WaveFlowLogo";
import { StepHeading } from "./StepHeading";

/**
 * Whether the system language is one the app speaks. Frozen at mount: a
 * pick made on this screen must not turn the "we could not match your
 * language" hint off or on.
 */
function detectFallback(): boolean {
  const raw =
    (typeof navigator !== "undefined"
      ? (navigator.language ?? navigator.languages?.[0])
      : null) ?? "en";
  // The normaliser lands on "en" when it cannot match the locale; only a
  // locale that is not English to begin with is a real fallback.
  return (
    normalizeSupportedLanguageCode(raw) === "en" &&
    !raw.toLowerCase().startsWith("en")
  );
}

export function WelcomeStep({
  askName,
  profileName,
  onProfileNameChange,
  nameError,
}: {
  askName: boolean;
  profileName: string;
  onProfileNameChange: (value: string) => void;
  nameError: string | null;
}) {
  const { t, i18n } = useTranslation();
  const [fallback] = useState(detectFallback);

  return (
    <div className="mx-auto max-w-xl py-4">
      <WaveFlowLogo className="mb-6 h-12 w-12 text-emerald-500" />
      <StepHeading
        title={t("onboarding.welcome.title")}
        lede={t("onboarding.welcome.lede")}
      />

      <div className="mt-8 space-y-3">
        <div className="onboarding-tile flex items-center gap-4 rounded-2xl border border-zinc-200 px-4 py-3 dark:border-zinc-800">
          <Globe
            size={18}
            className="shrink-0 text-zinc-400"
            aria-hidden="true"
          />
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("onboarding.welcome.language")}
            </div>
            {fallback && (
              <p className="mt-0.5 flex items-start gap-1.5 text-xs text-amber-600 dark:text-amber-300/90">
                <AlertCircle
                  size={12}
                  className="mt-0.5 shrink-0"
                  aria-hidden="true"
                />
                {t("onboarding.welcome.languageFallback")}
              </p>
            )}
          </div>
          <LanguageDropdown
            currentCode={normalizeSupportedLanguageCode(
              i18n.resolvedLanguage ?? i18n.language,
            )}
            onSelect={(code) => {
              i18n.changeLanguage(code).catch((err) => {
                console.error("[Onboarding] changeLanguage failed", err);
              });
            }}
          />
        </div>

        {askName && (
          <label className="onboarding-tile flex items-center gap-4 rounded-2xl border border-zinc-200 px-4 py-3 dark:border-zinc-800">
            <UserRound
              size={18}
              className="shrink-0 text-zinc-400"
              aria-hidden="true"
            />
            <span className="min-w-0 flex-1">
              <span className="block text-sm font-medium text-zinc-900 dark:text-white">
                {t("onboarding.welcome.name")}
              </span>
              <span className="block text-xs text-zinc-500 dark:text-zinc-400">
                {t("onboarding.welcome.nameHint")}
              </span>
            </span>
            <input
              type="text"
              value={profileName}
              onChange={(e) => onProfileNameChange(e.target.value)}
              placeholder={t("onboarding.welcome.namePlaceholder")}
              autoComplete="off"
              spellCheck={false}
              maxLength={64}
              className="w-44 rounded-lg border border-zinc-200 bg-white px-3 py-2 text-sm text-zinc-900 placeholder:text-zinc-400 focus:border-emerald-500/40 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100"
            />
          </label>
        )}
        {nameError && (
          <p className="text-xs text-rose-500" role="alert">
            {nameError}
          </p>
        )}
      </div>
    </div>
  );
}
