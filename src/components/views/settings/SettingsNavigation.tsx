import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Database,
  Disc3,
  ImageIcon,
  Keyboard,
  Library,
  Palette,
  Puzzle,
  Search,
  Settings2,
  Stethoscope,
  X,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { SETTINGS_CATEGORIES, type SettingsCategory } from "./settingsCatalog";

const ICONS = {
  general: Settings2,
  library: Library,
  playback: Disc3,
  appearance: Palette,
  media: ImageIcon,
  integrations: Zap,
  plugins: Puzzle,
  shortcuts: Keyboard,
  data: Database,
  diagnostics: Stethoscope,
} satisfies Record<SettingsCategory, LucideIcon>;

function normalize(value: string) {
  return value.normalize("NFD").replace(/\p{M}/gu, "").toLocaleLowerCase();
}

interface SettingsNavigationProps {
  activeCategory: SettingsCategory;
  onSelect: (category: SettingsCategory, group?: string) => void;
}

export function SettingsNavigation({
  activeCategory,
  onSelect,
}: SettingsNavigationProps) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const searchId = useId();
  const categoryId = useId();
  const terms = normalize(query).trim().split(/\s+/).filter(Boolean);
  const results = terms.length
    ? SETTINGS_CATEGORIES.flatMap((category) =>
        category.groups
          .filter((group) => {
            const text = normalize(
              [
                t(category.labelKey),
                t(group.labelKey),
                ...group.searchKeys.map((key) => t(key)),
              ].join(" "),
            );
            return terms.every((term) => text.includes(term));
          })
          .map((group) => ({ category, group })),
      )
    : [];

  return (
    <aside className="min-w-0 @4xl:sticky @4xl:top-6 @4xl:max-h-[calc(100dvh-12rem)] @4xl:overflow-y-auto">
      <div className="relative mb-5">
        <label htmlFor={searchId} className="sr-only">
          {t("settings.organization.search")}
        </label>
        <Search
          size={16}
          aria-hidden="true"
          className="pointer-events-none absolute start-3 top-3 text-zinc-400"
        />
        <input
          id={searchId}
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setQuery("");
          }}
          placeholder={t("settings.organization.search")}
          className="h-10 w-full rounded-lg border border-zinc-200 bg-white ps-9 pe-9 text-sm text-zinc-800 outline-none focus:border-emerald-500 focus:ring-2 focus:ring-emerald-500/20 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100 [&::-webkit-search-cancel-button]:appearance-none"
        />
        {query && (
          <button
            type="button"
            onClick={() => setQuery("")}
            aria-label={t("settings.organization.clear")}
            className="absolute end-1 top-1 rounded-md p-2 text-zinc-500 hover:bg-zinc-100 focus-visible:outline-emerald-500 dark:hover:bg-zinc-800"
          >
            <X size={16} />
          </button>
        )}
      </div>

      {terms.length > 0 ? (
        <nav
          aria-label={t("settings.organization.searchResults")}
          className="max-h-[55dvh] space-y-1 overflow-y-auto"
        >
          <p
            role="status"
            className="px-3 pb-2 text-xs text-zinc-500 dark:text-zinc-400"
          >
            {results.length
              ? `${t("settings.organization.searchResults")} (${results.length})`
              : t("settings.organization.noResults")}
          </p>
          {results.map(({ category, group }) => (
            <button
              key={group.id}
              type="button"
              onClick={() => {
                onSelect(category.id, group.id);
                setQuery("");
              }}
              className="block w-full rounded-lg px-3 py-2.5 text-start text-sm text-zinc-800 hover:bg-zinc-100 focus-visible:outline-emerald-500 dark:text-zinc-100 dark:hover:bg-zinc-800"
            >
              <span className="block text-xs text-zinc-500 dark:text-zinc-400">
                {t(category.labelKey)}
              </span>
              <span className="mt-0.5 block font-medium">
                {t(group.labelKey)}
              </span>
            </button>
          ))}
        </nav>
      ) : (
        <>
          <div className="@4xl:hidden">
            <label htmlFor={categoryId} className="sr-only">
              {t("settings.categoryNavLabel")}
            </label>
            <select
              id={categoryId}
              value={activeCategory}
              onChange={(event) =>
                onSelect(event.target.value as SettingsCategory)
              }
              className="w-full rounded-lg border border-zinc-200 bg-white px-3 py-2.5 text-sm text-zinc-800 focus-visible:outline-emerald-500 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100"
            >
              {SETTINGS_CATEGORIES.map((category) => (
                <option key={category.id} value={category.id}>
                  {t(category.labelKey)}
                </option>
              ))}
            </select>
          </div>
          <nav
            aria-label={t("settings.categoryNavLabel")}
            className="hidden space-y-1 @4xl:block"
          >
            {SETTINGS_CATEGORIES.map(({ id, labelKey }) => {
              const Icon = ICONS[id];
              const active = id === activeCategory;
              return (
                <button
                  key={id}
                  type="button"
                  aria-current={active ? "page" : undefined}
                  onClick={() => onSelect(id)}
                  className={`flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-start text-sm font-medium transition-colors focus-visible:outline-emerald-500 ${active ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-300" : "text-zinc-600 hover:bg-zinc-100 hover:text-zinc-900 dark:text-zinc-400 dark:hover:bg-zinc-800 dark:hover:text-zinc-100"}`}
                >
                  <Icon size={17} aria-hidden="true" className="shrink-0" />
                  {t(labelKey)}
                </button>
              );
            })}
          </nav>
        </>
      )}
    </aside>
  );
}
