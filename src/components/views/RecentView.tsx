import { useTranslation } from "react-i18next";
import { Clock } from "lucide-react";
import { EmptyState } from "../common/EmptyState";

export function RecentView() {
  const { t } = useTranslation();

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-6">
        <div className="w-24 h-24 rounded-2xl bg-blue-100 text-blue-500 flex items-center justify-center shadow-sm dark:bg-blue-950/60 dark:text-blue-400">
          <Clock size={48} />
        </div>
        <div>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("recent.badge")}
          </div>
          <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
            {t("recent.title")}
          </h1>
          <div className="text-sm text-zinc-500 mt-1">
            {t("recent.count", { count: 0 })}
          </div>
        </div>
      </div>

      {/* Empty State */}
      <EmptyState
        icon={<Clock size={40} />}
        title={t("recent.emptyTitle")}
        description={t("recent.emptyDescription")}
        accent="blue"
        shape="circle"
        className="py-20"
      />
    </div>
  );
}
