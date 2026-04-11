import {
  Library,
  Music2,
  Disc,
  Mic2,
  Tags,
  Folder,
  Share,
  RefreshCcw,
  Image as ImageIcon,
  Edit2,
  Trash2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { LibraryTab } from "../../types";
import { Tab } from "../common/Tab";
import { IconButton } from "../common/IconButton";
import { EmptyState } from "../common/EmptyState";
import { UploadIcon } from "../common/Icons";

interface LibraryViewProps {
  activeTab: LibraryTab;
  setActiveTab: (tab: LibraryTab) => void;
}

const tabConfig: { id: LibraryTab; icon: typeof Music2 }[] = [
  { id: "morceaux", icon: Music2 },
  { id: "albums", icon: Disc },
  { id: "artistes", icon: Mic2 },
  { id: "genres", icon: Tags },
  { id: "dossiers", icon: Folder },
];

const emptyStateIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  dossiers: Folder,
};

const headerIcons: Record<LibraryTab, typeof Music2> = {
  morceaux: Music2,
  albums: Disc,
  artistes: Mic2,
  genres: Tags,
  dossiers: Folder,
};

export function LibraryView({ activeTab, setActiveTab }: LibraryViewProps) {
  const { t } = useTranslation();
  const EmptyIcon = emptyStateIcons[activeTab];
  const HeaderIcon = headerIcons[activeTab];
  const headerSubtext =
    activeTab === "dossiers"
      ? t("library.header.subtext.dossiers")
      : t(`library.header.subtext.${activeTab}`, { count: 0 });

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-start justify-between">
        <div className="flex items-center space-x-6">
          <div className="w-24 h-24 rounded-2xl bg-emerald-100 text-emerald-600 dark:bg-emerald-950/60 dark:text-emerald-400 flex items-center justify-center shadow-sm">
            <Library size={48} />
          </div>
          <div>
            <h1 className="text-4xl font-bold mb-2 text-zinc-900 dark:text-white">
              lofi Base
            </h1>
            <div className="flex items-center text-sm text-zinc-500 space-x-2">
              <HeaderIcon size={16} />
              <span>{headerSubtext}</span>
            </div>
          </div>
        </div>

        <div className="flex items-center space-x-3">
          <button className="bg-emerald-500 hover:bg-emerald-600 text-white px-4 py-2.5 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm shadow-emerald-500/30">
            <Folder size={16} />
            <span>{t("library.header.addFolder")}</span>
          </button>

          <div className="flex items-center space-x-1 p-1 rounded-xl border border-zinc-200 bg-white shadow-sm dark:border-zinc-800 dark:bg-zinc-800/50">
            <IconButton icon={<Share size={18} />} />
            <IconButton icon={<RefreshCcw size={18} />} />
            <IconButton icon={<ImageIcon size={18} />} />
            <IconButton icon={<Edit2 size={18} />} />
            <IconButton
              icon={<Trash2 size={18} />}
              className="text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-500/10"
            />
          </div>
        </div>
      </div>

      {/* Tabs */}
      <div className="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800">
        <div className="flex space-x-6">
          {tabConfig.map((tab) => (
            <Tab
              key={tab.id}
              active={activeTab === tab.id}
              icon={<tab.icon size={18} />}
              label={t(`library.tabs.${tab.id}`)}
              onClick={() => setActiveTab(tab.id)}
            />
          ))}
        </div>

        {/* View toggles */}
        <div className="flex items-center space-x-1 mb-2">
          <button className="p-1.5 rounded-md bg-zinc-200 text-zinc-800 dark:bg-zinc-700 dark:text-white">
            <svg
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
            >
              <rect x="3" y="3" width="7" height="7" />
              <rect x="14" y="3" width="7" height="7" />
              <rect x="14" y="14" width="7" height="7" />
              <rect x="3" y="14" width="7" height="7" />
            </svg>
          </button>
          <button className="p-1.5 rounded-md text-zinc-400 hover:bg-zinc-100 dark:text-zinc-500 dark:hover:bg-zinc-800">
            <svg
              width="18"
              height="18"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
            >
              <line x1="8" y1="6" x2="21" y2="6" />
              <line x1="8" y1="12" x2="21" y2="12" />
              <line x1="8" y1="18" x2="21" y2="18" />
              <line x1="3" y1="6" x2="3.01" y2="6" />
              <line x1="3" y1="12" x2="3.01" y2="12" />
              <line x1="3" y1="18" x2="3.01" y2="18" />
            </svg>
          </button>
        </div>
      </div>

      {/* Empty State */}
      <EmptyState
        icon={<EmptyIcon size={40} />}
        title={t(`library.empty.${activeTab}.title`)}
        description={t(`library.empty.${activeTab}.description`)}
        className="py-20"
      >
        <div className="mt-8 flex items-center flex-wrap justify-center gap-4">
          <button className="bg-emerald-500 hover:bg-emerald-600 text-white px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm">
            <UploadIcon size={18} />
            <span>{t("library.actions.importFiles")}</span>
          </button>
          <button className="px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors border border-zinc-200 bg-white hover:bg-zinc-50 text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:hover:bg-zinc-700 dark:text-zinc-300">
            <Folder size={18} />
            <span>{t("library.actions.importFolder")}</span>
          </button>
        </div>
      </EmptyState>
    </div>
  );
}
