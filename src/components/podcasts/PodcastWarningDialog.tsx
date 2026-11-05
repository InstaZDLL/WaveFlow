import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Eye, HardDrive, Power, RefreshCw, Server } from "lucide-react";

import { usePodcastsEnabled } from "../../hooks/usePodcastsEnabled";
import { PodcastDialogShell } from "./PodcastDialogShell";

const POINTS = [
  { id: "hosts", Icon: Eye },
  { id: "checks", Icon: RefreshCw },
  { id: "downloads", Icon: HardDrive },
  { id: "server", Icon: Server },
  { id: "stop", Icon: Power },
] as const;

interface PodcastWarningDialogProps {
  isOpen: boolean;
  onClose: () => void;
  onAccepted: () => void;
}

/**
 * What changes when podcasts turn on, in plain words, before anything does.
 * **Its confirmation is the only caller of `acceptWarning`** (RFC-009): Cancel,
 * Escape and the backdrop leave the switch alone, and the focus starts on
 * Cancel. A profile imported comes back off, and this shows again.
 */
export function PodcastWarningDialog({
  isOpen,
  onClose,
  onAccepted,
}: PodcastWarningDialogProps) {
  const { t } = useTranslation();
  const { enabled, acceptWarning } = usePodcastsEnabled();
  const [busy, setBusy] = useState(false);
  const [wrote, setWrote] = useState(false);

  const [lastIsOpen, setLastIsOpen] = useState(isOpen);
  if (lastIsOpen !== isOpen) {
    setLastIsOpen(isOpen);
    setBusy(false);
    setWrote(false);
  }

  // The hook takes a refused write back rather than rejecting, so what the
  // switch reads once the write has settled is what says it worked. The dialog
  // then stays open and says so, since nothing has changed.
  useEffect(() => {
    if (wrote && enabled) onAccepted();
  }, [wrote, enabled, onAccepted]);

  const confirm = async () => {
    setBusy(true);
    setWrote(false);
    try {
      await acceptWarning();
      setWrote(true);
    } finally {
      setBusy(false);
    }
  };

  return (
    <PodcastDialogShell
      isOpen={isOpen}
      mark="warning"
      title={t("podcasts.warning.title")}
      onClose={onClose}
      confirm={{
        label: t("podcasts.warning.confirm"),
        busy,
        onConfirm: () => void confirm(),
      }}
    >
      <p className="text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
        {t("podcasts.warning.lead")}
      </p>
      <ul className="space-y-3">
        {POINTS.map(({ id, Icon }) => (
          <li key={id} className="flex gap-3">
            <Icon
              size={16}
              className="mt-0.5 shrink-0 text-zinc-400"
              aria-hidden="true"
            />
            <div className="min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t(`podcasts.warning.points.${id}.title`)}
              </div>
              <p className="mt-0.5 text-sm leading-relaxed text-zinc-600 dark:text-zinc-400">
                {t(`podcasts.warning.points.${id}.body`)}
              </p>
            </div>
          </li>
        ))}
      </ul>
      {wrote && !enabled && (
        <div
          role="alert"
          className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-600 dark:border-red-500/30 dark:bg-red-500/10 dark:text-red-400"
        >
          {t("podcasts.warning.failed")}
        </div>
      )}
    </PodcastDialogShell>
  );
}
