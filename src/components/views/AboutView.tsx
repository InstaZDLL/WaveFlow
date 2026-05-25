import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import {
  ArrowLeft,
  ExternalLink,
  Database,
  Zap,
  Music,
  Waves,
  Settings2,
  Sparkles,
  Wrench,
  AlertTriangle,
  GitCommit,
} from "lucide-react";
import type { ViewId } from "../../types";
import { WaveFlowLogo } from "../common/WaveFlowLogo";
import { getChangelog, type ChangelogEntry } from "../../lib/tauri/changelog";
import {
  comboParts,
  DEFAULT_BINDINGS,
  loadBindings,
  SHORTCUT_ACTIONS,
  SHORTCUTS_CHANGED_EVENT,
  type ShortcutAction,
  type ShortcutBindings,
} from "../../lib/shortcuts";

interface AboutViewProps {
  onNavigate: (view: ViewId) => void;
}

function TechItem({
  icon,
  name,
  version,
}: {
  icon?: React.ReactNode;
  name: string;
  version: string;
}) {
  return (
    <div className="px-4 py-3 flex items-center justify-between">
      <div className="flex items-center space-x-2">
        {icon && <span className="text-zinc-400">{icon}</span>}
        <span className="text-sm text-zinc-700 dark:text-zinc-300">{name}</span>
      </div>
      <span className="text-sm text-zinc-400 font-mono">{version}</span>
    </div>
  );
}

function ShortcutRow({ label, combo }: { label: string; combo: string }) {
  const { t } = useTranslation();
  const parts = comboParts(combo);
  return (
    <div className="px-4 py-3 flex items-center justify-between">
      <span className="text-sm text-zinc-700 dark:text-zinc-300">{label}</span>
      {parts.length === 0 ? (
        <span className="text-xs italic text-zinc-400">
          {t("settings.shortcuts.unbound")}
        </span>
      ) : (
        <div className="flex items-center space-x-1">
          {parts.map((part, i) => (
            <span key={i}>
              {i > 0 && <span className="text-zinc-400 mx-1">+</span>}
              <kbd className="px-2 py-1 text-xs font-mono rounded border border-zinc-200 bg-zinc-50 text-zinc-600 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-400">
                {part}
              </kbd>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

function ShortcutsList() {
  const { t } = useTranslation();
  const [bindings, setBindings] = useState<ShortcutBindings>(DEFAULT_BINDINGS);
  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      loadBindings()
        .then((b) => {
          if (!cancelled) setBindings(b);
        })
        .catch(() => {});
    };
    refresh();
    window.addEventListener(SHORTCUTS_CHANGED_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(SHORTCUTS_CHANGED_EVENT, refresh);
    };
  }, []);
  return (
    <div className="space-y-0">
      {SHORTCUT_ACTIONS.map((action: ShortcutAction) => (
        <ShortcutRow
          key={action}
          label={t(`settings.shortcuts.actions.${action}`)}
          combo={bindings[action]}
        />
      ))}
    </div>
  );
}

/** How many entries to show before "Show more" expands the rest. */
const CHANGELOG_VISIBLE = 25;

function changelogIcon(kind: string) {
  if (kind === "feat")
    return <Sparkles size={14} className="text-emerald-500" />;
  if (kind === "fix") return <Wrench size={14} className="text-amber-500" />;
  return <GitCommit size={14} className="text-zinc-400" />;
}

function ChangelogSection() {
  const { t, i18n } = useTranslation();
  const [entries, setEntries] = useState<ChangelogEntry[] | null>(null);
  const [error, setError] = useState(false);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    getChangelog()
      .then((rows) => {
        if (cancelled) return;
        // Hide noise: keep only the user-facing types.
        const visible = rows.filter((r) =>
          ["feat", "fix", "perf", "refactor"].includes(r.type),
        );
        setEntries(visible);
      })
      .catch((err) => {
        console.error("[About] changelog load failed", err);
        if (!cancelled) setError(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (error) return null;
  if (entries == null) {
    return (
      <p className="text-xs text-zinc-400 px-4">
        {t("about.changelog.loading")}
      </p>
    );
  }
  if (entries.length === 0) {
    return (
      <p className="text-xs text-zinc-400 px-4">{t("about.changelog.empty")}</p>
    );
  }

  const shown = expanded ? entries : entries.slice(0, CHANGELOG_VISIBLE);
  const dateFormatter = new Intl.DateTimeFormat(
    i18n.resolvedLanguage ?? i18n.language,
    {
      day: "2-digit",
      month: "short",
      year: "numeric",
    },
  );

  return (
    <div className="space-y-1">
      <ul className="space-y-2">
        {shown.map((entry) => (
          <li
            key={entry.hash}
            className="flex items-start gap-3 px-4 py-2 rounded-lg hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors"
          >
            <span className="mt-1 shrink-0">{changelogIcon(entry.type)}</span>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-[10px] font-bold uppercase tracking-wide text-zinc-400">
                  {entry.type}
                </span>
                {entry.scope && (
                  <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-zinc-100 dark:bg-zinc-800 text-zinc-500">
                    {entry.scope}
                  </span>
                )}
                {entry.breaking && (
                  <span
                    className="inline-flex items-center gap-1 text-[10px] font-bold uppercase text-red-500"
                    title={t("about.changelog.breaking") ?? "Breaking change"}
                  >
                    <AlertTriangle size={10} aria-hidden="true" />
                    {t("about.changelog.breaking")}
                  </span>
                )}
              </div>
              <p className="text-sm text-zinc-700 dark:text-zinc-300 leading-snug">
                {entry.subject}
              </p>
            </div>
            <span className="shrink-0 text-[11px] text-zinc-400 font-mono mt-0.5">
              {dateFormatter.format(new Date(entry.date))}
            </span>
          </li>
        ))}
      </ul>
      {entries.length > CHANGELOG_VISIBLE && (
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="w-full text-center text-xs font-medium text-emerald-600 hover:text-emerald-700 dark:text-emerald-400 dark:hover:text-emerald-300 py-2"
        >
          {expanded
            ? t("about.changelog.collapse")
            : t("about.changelog.expand", {
                count: entries.length - CHANGELOG_VISIBLE,
              })}
        </button>
      )}
    </div>
  );
}

export function AboutView({ onNavigate }: AboutViewProps) {
  const { t } = useTranslation();
  // Read the version from Tauri at runtime so the bundled tauri.conf.json
  // stays the source of truth — no more cross-file drift after a bump.
  const [version, setVersion] = useState<string>("");
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch((err) => console.error("[AboutView] getVersion failed", err));
  }, []);

  return (
    <div className="max-w-4xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-4">
        <button
          type="button"
          onClick={() => onNavigate("home")}
          aria-label={t("common.back")}
          className="p-1 rounded-lg text-zinc-400 hover:text-zinc-800 dark:hover:text-white transition-colors"
        >
          <ArrowLeft size={20} />
        </button>
        <div>
          <h1 className="text-3xl font-bold text-zinc-900 dark:text-white">
            {t("about.title")}
          </h1>
          <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
        </div>
      </div>

      {/* Hero Card */}
      <div className="relative overflow-hidden rounded-3xl p-10 bg-linear-to-br from-zinc-900 to-zinc-800 text-white">
        <div className="absolute top-0 right-0 text-[120px] font-black text-white/5 leading-none pr-8 pt-4 select-none">
          {version}
        </div>
        <div className="relative z-10">
          <div className="flex items-center space-x-2 mb-2">
            <WaveFlowLogo className="w-8 h-8" />
            <h2 className="text-3xl font-black">
              Wave<span className="text-emerald-400">Flow</span>
            </h2>
          </div>
          <p className="text-zinc-400 mb-4">{t("about.hero.subtitle")}</p>
          <div className="flex items-center space-x-3 mb-4">
            <span className="bg-emerald-500 text-white text-xs px-2.5 py-1 rounded-full font-semibold">
              v{version}
            </span>
            <span className="text-zinc-500 text-sm">2026</span>
            <span className="text-zinc-600">·</span>
            <span className="text-zinc-500 text-sm">GPL-3.0 License</span>
          </div>
          <button
            type="button"
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-700 bg-zinc-800 text-sm font-medium text-zinc-300 hover:bg-zinc-700 transition-colors"
          >
            <ExternalLink size={14} aria-hidden="true" />
            <span>{t("about.hero.checkUpdates")}</span>
          </button>
          <p className="text-xs text-zinc-500 mt-4">
            {t("about.hero.developedBy")}{" "}
            <span className="font-semibold text-zinc-300">WaveFlow Team</span>
          </p>
        </div>
      </div>

      {/* Framework Desktop */}
      <section aria-labelledby="about-framework-heading">
        <h2
          id="about-framework-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.frameworkDesktop")}
        </h2>
        <TechItem name="Tauri" version="2.x" />
      </section>

      {/* Frontend */}
      <section aria-labelledby="about-frontend-heading">
        <h2
          id="about-frontend-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.frontend")}
        </h2>
        <div className="grid grid-cols-2 gap-x-8">
          <TechItem name="React" version="19.x" />
          <TechItem name="Vite" version="8.x" />
          <TechItem name="TypeScript" version="6.x" />
          <TechItem name="Tailwind CSS" version="4.x" />
        </div>
      </section>

      {/* Backend (Rust) */}
      <section aria-labelledby="about-backend-heading">
        <h2
          id="about-backend-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.backend")}
        </h2>
        <div className="grid grid-cols-2 gap-x-8">
          <TechItem name="Rust" version="2021 ed." />
          <TechItem icon={<Database size={16} />} name="SQLx" version="0.8" />
          <TechItem name="SQLite" version="3.x" />
          <TechItem icon={<Zap size={16} />} name="Tokio" version="1.x" />
        </div>
      </section>

      {/* Audio */}
      <section aria-labelledby="about-audio-heading">
        <h2
          id="about-audio-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.audio")}
        </h2>
        <div className="grid grid-cols-2 gap-x-8">
          <TechItem icon={<Music size={16} />} name="Symphonia" version="0.6" />
          <TechItem icon={<Settings2 size={16} />} name="CPAL" version="0.17" />
          <TechItem icon={<Waves size={16} />} name="Rubato" version="3.0" />
        </div>
      </section>

      {/* Raccourcis clavier (dynamiques — reflètent les bindings
          courants modifiés depuis Paramètres). */}
      <section aria-labelledby="about-shortcuts-heading">
        <h2
          id="about-shortcuts-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.shortcuts")}
        </h2>
        <ShortcutsList />
      </section>

      {/* Changelog généré depuis les commits conventional */}
      <section aria-labelledby="about-changelog-heading">
        <h2
          id="about-changelog-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.changelog")}
        </h2>
        <ChangelogSection />
      </section>

      {/* Crédits */}
      <section aria-labelledby="about-credits-heading">
        <h2
          id="about-credits-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.credits")}
        </h2>
        <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800/50 p-5">
          <p className="text-sm text-zinc-700 dark:text-zinc-300 font-medium">
            {t("about.credits.text")}
          </p>
          <p className="text-xs text-zinc-400 mt-1">
            {t("about.credits.icons")}
          </p>
        </div>
      </section>
    </div>
  );
}
