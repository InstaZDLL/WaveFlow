import { Suspense, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import {
  IMPORT_PODCAST_OPML_EVENT,
  OPEN_PODCAST_WARNING_EVENT,
  openView,
  type PodcastWarningRequest,
} from "../../lib/discovery";
import { retryableLazy } from "../../lib/preloadable";
import {
  podcastErrorWord,
  podcastImportOpml,
  type PodcastOpmlImportReport,
} from "../../lib/tauri/podcast";

const PodcastWarningDialog = retryableLazy(() =>
  import("./PodcastWarningDialog").then(
    (module) => module.PodcastWarningDialog,
  ),
);
const PodcastOpmlReportDialog = retryableLazy(() =>
  import("./PodcastOpmlReportDialog").then(
    (module) => module.PodcastOpmlReportDialog,
  ),
);

/**
 * The podcast dialogs that do not belong to a page: the warning, asked for from
 * Settings, What's new or the page itself, and an OPML import's report, which
 * has to reach someone who left the page during a long list. Mounted once, by
 * the layout.
 */
export function PodcastDialogsHost() {
  const { t } = useTranslation();
  const [warningShown, setWarningShown] = useState(false);
  const [warningOpen, setWarningOpen] = useState(false);
  const openPageRef = useRef(false);
  const [report, setReport] = useState<PodcastOpmlImportReport | null>(null);
  const [reportOpen, setReportOpen] = useState(false);

  useEffect(() => {
    const onOpen = (event: Event) => {
      const request = (event as CustomEvent<PodcastWarningRequest>).detail;
      openPageRef.current = request?.openPage ?? false;
      setWarningShown(true);
      setWarningOpen(true);
    };
    window.addEventListener(OPEN_PODCAST_WARNING_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_PODCAST_WARNING_EVENT, onOpen);
  }, []);

  useEffect(() => {
    const onImport = (event: Event) => {
      const path = (event as CustomEvent<string>).detail;
      podcastImportOpml(path)
        .then(setReport)
        .catch((err) => {
          const word = podcastErrorWord(err);
          if (word == null) console.error("[podcasts] OPML import failed", err);
          // The file itself was refused: one line, under the file's name for it.
          setReport({
            outcome: "done",
            added: 0,
            already: 0,
            total: 0,
            failed: [
              { title: t("podcasts.opml.report.theFile"), code: word ?? "" },
            ],
          });
        })
        .finally(() => setReportOpen(true));
    };
    window.addEventListener(IMPORT_PODCAST_OPML_EVENT, onImport);
    return () =>
      window.removeEventListener(IMPORT_PODCAST_OPML_EVENT, onImport);
  }, [t]);

  const closeWarning = useCallback(() => setWarningOpen(false), []);
  const onAccepted = useCallback(() => {
    setWarningOpen(false);
    if (openPageRef.current) openView({ view: "podcasts" });
  }, []);

  return (
    <>
      {warningShown && (
        <Suspense fallback={null}>
          <PodcastWarningDialog
            isOpen={warningOpen}
            onClose={closeWarning}
            onAccepted={onAccepted}
          />
        </Suspense>
      )}
      {report && (
        <Suspense fallback={null}>
          <PodcastOpmlReportDialog
            report={report}
            isOpen={reportOpen}
            onClose={() => setReportOpen(false)}
          />
        </Suspense>
      )}
    </>
  );
}
