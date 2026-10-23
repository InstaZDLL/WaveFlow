import { useCallback, useContext, useState } from "react";
import { useTranslation } from "react-i18next";
import { RotateCcw } from "lucide-react";

import { SETTINGS_CATEGORIES, type SettingsCategory } from "./settingsCatalog";
import {
  resetCategory,
  SettingsResetScope,
  useCategoryModifications,
  useSettingsResetContext,
} from "./settingsReset";
import { SettingsResetModal } from "./SettingsResetModal";

/**
 * Title and one-line description at the top of a settings category. Both
 * strings come from the catalog entry, the same one the navigation and the
 * settings search read, so a label or description changed there shows up
 * here too instead of drifting from a key hardcoded in the view.
 *
 * Beside them, how many of the page's settings differ from their default
 * ("3 modified") and the action that puts them back, behind a dialog that
 * lists them. Nothing shows while the page is at its defaults.
 */
export function SettingsCategoryHeader({
  category,
}: {
  category: SettingsCategory;
}) {
  const { t } = useTranslation();
  const ctx = useSettingsResetContext();
  const { modifications, refresh } = useCategoryModifications(category, ctx);
  const onCategoryReset = useContext(SettingsResetScope);
  const [dialogOpen, setDialogOpen] = useState(false);

  const handleConfirm = useCallback(async () => {
    const { failed } = await resetCategory(category, ctx);
    if (failed.length === 0) {
      setDialogOpen(false);
      onCategoryReset(category);
    } else {
      // Keep the dialog on what is still left; the list re-reads.
      refresh();
    }
    return failed.length;
  }, [category, ctx, onCategoryReset, refresh]);

  const total = modifications?.total ?? 0;
  // Close once nothing is left to reset: what was counted may have been
  // put back from elsewhere while the dialog was open.
  if (dialogOpen && modifications && total === 0) setDialogOpen(false);

  const entry = SETTINGS_CATEGORIES.find((c) => c.id === category);
  if (!entry) return null;
  const label = t(entry.labelKey);

  return (
    <header className="settings-category-header flex flex-wrap items-start justify-between gap-x-6 gap-y-3">
      <div className="min-w-0">
        <h2
          id={`settings-heading-${category}`}
          className="text-xl font-semibold text-zinc-900 dark:text-white"
        >
          {label}
        </h2>
        <p className="mt-1 text-sm text-zinc-500 dark:text-zinc-400">
          {t(entry.descriptionKey)}
        </p>
      </div>
      {/* Always mounted, so a count appearing is announced. */}
      <div aria-live="polite" className="settings-modified shrink-0">
        {total > 0 && (
          <div className="flex items-center gap-2">
            <span className="settings-modified-count text-sm tabular-nums text-zinc-500 dark:text-zinc-400">
              {t("settings.modified.count", { count: total })}
            </span>
            <span
              aria-hidden="true"
              className="settings-modified-dot text-zinc-300 dark:text-zinc-600"
            >
              ·
            </span>
            <button
              type="button"
              onClick={() => setDialogOpen(true)}
              aria-haspopup="dialog"
              aria-label={t("settings.modified.resetAria", {
                category: label,
              })}
              className="settings-modified-reset inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm font-medium text-zinc-700 transition-colors hover:bg-zinc-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 dark:text-zinc-200 dark:hover:bg-zinc-800"
            >
              <RotateCcw size={14} aria-hidden="true" />
              {t("settings.modified.reset")}
            </button>
          </div>
        )}
      </div>
      <SettingsResetModal
        isOpen={dialogOpen}
        categoryLabel={label}
        items={modifications?.items ?? []}
        total={total}
        onClose={() => setDialogOpen(false)}
        onConfirm={handleConfirm}
      />
    </header>
  );
}
