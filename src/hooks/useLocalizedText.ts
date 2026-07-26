import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { normalizeSupportedLanguageCode } from "../i18n";
import { resolveLocalizedText, type LocalizedText } from "../lib/localizedText";

/**
 * Resolver for plugin-authored {@link LocalizedText} values, bound to
 * the active app language.
 *
 * Goes through `useTranslation` rather than reading `i18n.language`
 * directly so the consuming component re-subscribes to i18next's
 * `languageChanged` event — a language switch re-renders the plugin
 * rows just like it re-renders `t()` output around them.
 *
 * The code is normalised first (`fr-FR` → `fr`, `kr` → `ko`) so the
 * lookup sees one of the app's 17 canonical codes, matching what a
 * plugin author is told to key their manifest on.
 */
export function useLocalizedText() {
  const { i18n } = useTranslation();
  const lang = normalizeSupportedLanguageCode(
    i18n.resolvedLanguage ?? i18n.language,
  );
  return useCallback(
    (value: LocalizedText | null | undefined) =>
      resolveLocalizedText(value, lang),
    [lang],
  );
}
