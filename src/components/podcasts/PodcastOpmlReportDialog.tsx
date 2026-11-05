import { useTranslation } from "react-i18next";

import { podcastFailureMessage } from "../../lib/podcastWords";
import type { PodcastOpmlImportReport } from "../../lib/tauri/podcast";
import { PodcastDialogShell } from "./PodcastDialogShell";

/** The lines listed: past these, a count. A list holds up to 2 000 feeds. */
const MAX_LISTED = 50;

/** How an OPML import ended: what was added, and each feed that was not. */
export function PodcastOpmlReportDialog({
  report,
  isOpen,
  onClose,
}: {
  report: PodcastOpmlImportReport;
  isOpen: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const refused = report.failed.some(
    (entry) => entry.code === "address_refused",
  );
  return (
    <PodcastDialogShell
      isOpen={isOpen}
      mark="opml-report"
      title={t("podcasts.opml.report.title")}
      closeLabel={t("podcasts.dialog.close")}
      onClose={onClose}
    >
      {report.outcome !== "done" && (
        <p className="text-sm font-medium text-zinc-900 dark:text-white">
          {t(`podcasts.opml.report.outcome.${report.outcome}`)}
        </p>
      )}
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        {t("podcasts.opml.report.summary", {
          added: report.added,
          already: report.already,
          failed: report.failed.length,
          total: report.total,
        })}
      </p>
      {refused && (
        <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
          {t("podcasts.opml.publicOnly")}
        </p>
      )}
      {report.failed.length > 0 && (
        <ul className="max-h-56 space-y-1.5 overflow-y-auto text-sm">
          {report.failed.slice(0, MAX_LISTED).map((entry, index) => (
            <li key={index} className="min-w-0">
              <div className="truncate font-medium text-zinc-800 dark:text-zinc-200">
                {entry.title || t("podcasts.untitledChannel")}
              </div>
              <div className="text-xs text-zinc-500 dark:text-zinc-400">
                {podcastFailureMessage(t, entry.code)}
              </div>
            </li>
          ))}
        </ul>
      )}
      {report.failed.length > MAX_LISTED && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("podcasts.opml.report.more", {
            n: report.failed.length - MAX_LISTED,
          })}
        </p>
      )}
    </PodcastDialogShell>
  );
}
