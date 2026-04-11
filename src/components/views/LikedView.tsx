import { useTranslation } from "react-i18next";
import { Heart } from "lucide-react";
import { EmptyState } from "../common/EmptyState";

export function LikedView() {
  const { t } = useTranslation();

  return (
    <div className="max-w-6xl mx-auto space-y-8 animate-fade-in pb-20">
      {/* Header */}
      <div className="flex items-center space-x-6">
        <div className="w-24 h-24 rounded-2xl bg-pink-100 text-pink-500 flex items-center justify-center shadow-sm dark:bg-pink-950/60 dark:text-pink-400">
          <Heart size={48} className="fill-current" />
        </div>
        <div>
          <div className="text-[10px] font-bold tracking-widest text-zinc-400 uppercase mb-1">
            {t("liked.badge")}
          </div>
          <h1 className="text-4xl font-bold text-zinc-900 dark:text-white">
            {t("liked.title")}
          </h1>
          <div className="text-sm text-zinc-500 mt-1">
            {t("liked.count", { count: 0 })}
          </div>
        </div>
      </div>

      {/* Empty State */}
      <EmptyState
        icon={<Heart size={40} className="fill-current" />}
        title={t("liked.emptyTitle")}
        description={t("liked.emptyDescription")}
        accent="pink"
        shape="circle"
        className="py-20"
      />
    </div>
  );
}
