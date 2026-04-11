import { useTranslation } from "react-i18next";
import {
  ArrowLeft,
  ExternalLink,
  Database,
  Zap,
  Music,
  Waves,
  Settings2,
} from "lucide-react";
import type { ViewId } from "../../types";
import { WaveFlowLogo } from "../common/WaveFlowLogo";

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

function KeyboardShortcut({ label, keys }: { label: string; keys: string[] }) {
  return (
    <div className="px-4 py-3 flex items-center justify-between">
      <span className="text-sm text-zinc-700 dark:text-zinc-300">{label}</span>
      <div className="flex items-center space-x-1">
        {keys.map((key, i) => (
          <span key={i}>
            {i > 0 && <span className="text-zinc-400 mx-1">/</span>}
            <kbd className="px-2 py-1 text-xs font-mono rounded border border-zinc-200 bg-zinc-50 text-zinc-600 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-400">
              {key}
            </kbd>
          </span>
        ))}
      </div>
    </div>
  );
}

export function AboutView({ onNavigate }: AboutViewProps) {
  const { t } = useTranslation();

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
          0.1.0
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
              v0.1.0
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
          <TechItem name="Vite" version="6.x" />
          <TechItem name="TypeScript" version="5.x" />
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
          <TechItem icon={<Music size={16} />} name="Symphonia" version="0.5" />
          <TechItem icon={<Settings2 size={16} />} name="CPAL" version="0.17" />
          <TechItem icon={<Waves size={16} />} name="Rubato" version="1.0" />
        </div>
      </section>

      {/* Raccourcis clavier */}
      <section aria-labelledby="about-shortcuts-heading">
        <h2
          id="about-shortcuts-heading"
          className="text-[10px] font-bold tracking-widest text-zinc-400 mb-4 px-4 uppercase"
        >
          {t("about.sections.shortcuts")}
        </h2>
        <div className="space-y-0">
          <KeyboardShortcut
            label={t("about.shortcuts.playPause")}
            keys={[t("about.shortcuts.keys.space")]}
          />
          <KeyboardShortcut label={t("about.shortcuts.previousTrack")} keys={["←"]} />
          <KeyboardShortcut label={t("about.shortcuts.nextTrack")} keys={["→"]} />
          <KeyboardShortcut label={t("about.shortcuts.volume")} keys={["↑", "↓"]} />
          <KeyboardShortcut label={t("about.shortcuts.mute")} keys={["M"]} />
        </div>
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
          <p className="text-xs text-zinc-400 mt-1">{t("about.credits.icons")}</p>
        </div>
      </section>
    </div>
  );
}
