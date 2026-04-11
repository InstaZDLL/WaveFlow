import { useTranslation } from "react-i18next";
import { File, Folder, Library, Heart, Clock, ListMusic } from "lucide-react";
import type { ViewId } from "../../types";
import { ActionLink } from "../common/ActionLink";
import { StatCard } from "../common/StatCard";
import { EmptyState } from "../common/EmptyState";
import { UploadIcon, DownloadIcon } from "../common/Icons";

interface HomeViewProps {
  onNavigate: (view: ViewId) => void;
}

const WAVEFORM_BAR_COUNT = 80;
const WAVEFORM_HEIGHTS = Array.from({ length: WAVEFORM_BAR_COUNT }, (_, i) => {
  const x = i / (WAVEFORM_BAR_COUNT - 1);
  const wave =
    Math.sin(x * Math.PI * 5) * 0.55 +
    Math.sin(x * Math.PI * 11 + 0.6) * 0.3 +
    Math.sin(x * Math.PI * 19 + 1.2) * 0.18;
  const envelope = Math.sin(x * Math.PI);
  return Math.max(0.1, Math.abs(wave) * envelope * 0.95 + envelope * 0.15);
});

function getGreetingKey(): "morning" | "evening" | "night" {
  const hour = new Date().getHours();
  if (hour >= 5 && hour < 18) return "morning";
  if (hour >= 18 && hour < 23) return "evening";
  return "night";
}

export function HomeView({ onNavigate }: HomeViewProps) {
  const { t } = useTranslation();
  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Welcome Banner */}
      <div className="relative overflow-hidden rounded-3xl p-10 bg-linear-to-br from-emerald-50 to-white shadow-sm border border-emerald-100/50 dark:from-emerald-900/40 dark:to-zinc-800/40 dark:border-zinc-800 dark:shadow-none">
        {/* Ambient lights */}
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -top-24 -left-16 w-80 h-80 rounded-full bg-emerald-300/30 dark:bg-emerald-400/25 blur-3xl"
        />
        <div
          aria-hidden="true"
          className="pointer-events-none absolute -bottom-32 right-0 w-md h-112 rounded-full bg-emerald-400/20 dark:bg-emerald-500/20 blur-3xl"
        />

        <div className="relative">
          <div className="inline-flex items-center space-x-2 bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-500/40 dark:border-emerald-400/40 px-3 py-1 rounded-full text-xs font-semibold mb-6 backdrop-blur-sm">
            <div className="w-1.5 h-1.5 rounded-full bg-emerald-500 animate-pulse" />
            <span>{t("home.banner.badge")}</span>
          </div>

          <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
            {t(`home.greeting.${getGreetingKey()}`)}, Default
          </h1>
          <p className="text-zinc-500 dark:text-zinc-400 mb-8">
            {t("home.banner.subtitle")}
          </p>

          <div className="flex flex-wrap gap-6">
            <ActionLink icon={<File size={16} />} label={t("home.banner.openFile")} />
            <ActionLink
              icon={<Folder size={16} />}
              label={t("home.banner.openFolder")}
            />
            <ActionLink
              icon={<UploadIcon />}
              label={t("home.banner.importFiles")}
              highlight
            />
            <ActionLink
              icon={<DownloadIcon />}
              label={t("home.banner.importFolder")}
              highlight
            />
          </div>
        </div>
      </div>

      {/* Stats Cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <StatCard
          icon={<Library />}
          accent="emerald"
          count="1"
          label={t("home.stats.library")}
          onClick={() => onNavigate("library")}
        />
        <StatCard
          icon={<Heart className="fill-current" />}
          accent="pink"
          count="0"
          label={t("home.stats.liked")}
          onClick={() => onNavigate("liked")}
        />
        <StatCard
          icon={<Clock />}
          accent="blue"
          count="0"
          label={t("home.stats.recent")}
          onClick={() => onNavigate("recent")}
        />
        <StatCard
          icon={<ListMusic />}
          accent="purple"
          count="0"
          label={t("home.stats.playlists")}
        />
      </div>

      {/* Récemment joués Section */}
      <div>
        <h2 className="text-2xl font-bold mb-6 inline-block border-b-4 border-emerald-500 pb-1 text-zinc-900 dark:text-white">
          {t("home.recentlyPlayed.title")}
        </h2>
        <div className="relative overflow-hidden min-h-80 rounded-3xl border flex items-center justify-center p-8 border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/40 dark:shadow-none">
          <EmptyState
            icon={<Clock size={32} />}
            title={t("home.recentlyPlayed.emptyTitle")}
            description={t("home.recentlyPlayed.emptyDescription")}
            size="sm"
          >
            <svg
              viewBox="0 0 400 40"
              preserveAspectRatio="none"
              aria-hidden="true"
              className="mt-8 w-96 h-10 text-emerald-400 dark:text-emerald-400/60"
            >
              {WAVEFORM_HEIGHTS.map((h, i) => {
                const barWidth = 2.5;
                const gap = 2.5;
                const totalWidth =
                  WAVEFORM_BAR_COUNT * (barWidth + gap) - gap;
                const startX = (400 - totalWidth) / 2;
                const x = startX + i * (barWidth + gap);
                const barH = h * 36;
                const y = (40 - barH) / 2;
                return (
                  <rect
                    key={i}
                    x={x}
                    y={y}
                    width={barWidth}
                    height={barH}
                    rx={1}
                    fill="currentColor"
                  />
                );
              })}
            </svg>
          </EmptyState>
        </div>
      </div>
    </div>
  );
}
