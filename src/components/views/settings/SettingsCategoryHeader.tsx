import { useTranslation } from "react-i18next";

import { SETTINGS_CATEGORIES, type SettingsCategory } from "./settingsCatalog";

/**
 * Title and one-line description at the top of a settings category. Both
 * strings come from the catalog entry, the same one the navigation and the
 * settings search read, so a label or description changed there shows up
 * here too instead of drifting from a key hardcoded in the view.
 */
export function SettingsCategoryHeader({
  category,
}: {
  category: SettingsCategory;
}) {
  const { t } = useTranslation();
  const entry = SETTINGS_CATEGORIES.find((c) => c.id === category);
  if (!entry) return null;
  return (
    <header>
      <h2
        id={`settings-heading-${category}`}
        className="text-xl font-semibold text-zinc-900 dark:text-white"
      >
        {t(entry.labelKey)}
      </h2>
      <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
        {t(entry.descriptionKey)}
      </p>
    </header>
  );
}
