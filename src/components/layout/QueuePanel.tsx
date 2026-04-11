import { useTranslation } from "react-i18next";
import { X, ListMusic } from "lucide-react";
import { usePlayer } from "../../hooks/usePlayer";

export function QueuePanel() {
  const { t } = useTranslation();
  const { isQueueOpen, toggleQueue } = usePlayer();

  return (
    <div
      className={`absolute top-0 right-0 h-full w-80 shadow-2xl transform transition-transform duration-300 z-40 border-l bg-white border-zinc-200 text-zinc-800 dark:bg-zinc-900 dark:border-zinc-800 dark:text-zinc-100
        ${isQueueOpen ? "translate-x-0" : "translate-x-full"}`}
    >
      <div className="p-6 flex flex-col h-full">
        <div className="flex items-center justify-between mb-8">
          <div>
            <h2 className="text-xl font-bold">{t("queue.title")}</h2>
            <p className="text-xs text-zinc-500 mt-1">
              {t("queue.count", { count: 0 })}{" "}
              <span className="bg-zinc-200 dark:bg-zinc-700 text-[10px] px-2 py-0.5 rounded-full ml-2 font-medium">
                {t("queue.inactive")}
              </span>
            </p>
          </div>
          <button
            type="button"
            onClick={toggleQueue}
            aria-label={t("common.close")}
            className="p-2 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-full transition-colors"
          >
            <X size={20} />
          </button>
        </div>
        <div className="flex-1 flex flex-col items-center justify-center text-center">
          <div className="w-24 h-24 bg-zinc-100 dark:bg-zinc-800 rounded-2xl flex items-center justify-center mb-6 shadow-inner">
            <ListMusic size={40} className="text-zinc-300 dark:text-zinc-600" aria-hidden="true" />
          </div>
          <h3 className="font-semibold mb-2">{t("queue.emptyTitle")}</h3>
          <p className="text-sm text-zinc-500 max-w-50">
            {t("queue.emptyDescription")}
          </p>
        </div>
      </div>
    </div>
  );
}
