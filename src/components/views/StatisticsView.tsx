import { useTranslation } from "react-i18next";
import { ArrowLeft, BarChart2 } from "lucide-react";
import type { ViewId } from "../../types";
import { EmptyState } from "../common/EmptyState";

interface StatisticsViewProps {
  onNavigate: (view: ViewId) => void;
}

export function StatisticsView({ onNavigate }: StatisticsViewProps) {
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
            {t("statistics.title")}
          </h1>
          <div className="w-8 h-1 bg-emerald-500 rounded-full mt-1" />
        </div>
      </div>

      {/* Empty State */}
      <EmptyState
        icon={<BarChart2 size={40} />}
        title={t("statistics.emptyTitle")}
        description={t("statistics.emptyDescription")}
        className="py-32"
      />
    </div>
  );
}
