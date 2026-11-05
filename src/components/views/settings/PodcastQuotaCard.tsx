import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { HardDrive } from "lucide-react";

import { usePodcastDownloadQuota } from "../../../hooks/usePodcastDownloadQuota";
import { usePodcastsEnabled } from "../../../hooks/usePodcastsEnabled";
import { useProfile } from "../../../hooks/useProfile";
import {
  PODCAST_DOWNLOAD,
  podcastDownloadUsage,
  type PodcastDownloadPayload,
  type PodcastDownloadUsage,
} from "../../../lib/tauri/podcast";
import { formatBytes } from "../podcasts/format";

/**
 * Settings → Storage: the most this profile keeps of downloaded episodes. Absent
 * for someone who has no podcasts on and nothing kept, to whom it says nothing. A
 * lower quota removes no file: it stops the next download.
 */
export function PodcastQuotaCard() {
  const { t, i18n } = useTranslation();
  const { enabled } = usePodcastsEnabled();
  const quota = usePodcastDownloadQuota();
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  const [usage, setUsage] = useState<PodcastDownloadUsage | null>(null);

  // A download can end while this is on screen. Subscribes first, then reads, so
  // one ending between the two is heard rather than lost.
  useEffect(() => {
    let cancelled = false;
    let off: (() => void) | undefined;
    let latest = 0;
    const read = () => {
      const seq = ++latest;
      podcastDownloadUsage()
        .then((value) => {
          if (!cancelled && seq === latest) setUsage(value);
        })
        .catch((err) => {
          console.warn("[PodcastQuotaCard] reading the usage failed", err);
          if (!cancelled && seq === latest) setUsage(null);
        });
    };
    listen<PodcastDownloadPayload>(PODCAST_DOWNLOAD, ({ payload }) => {
      if (payload.state === "done") read();
    })
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[PodcastQuotaCard] listening failed", err))
      .finally(() => {
        if (!cancelled) read();
      });
    return () => {
      cancelled = true;
      off?.();
    };
  }, [activeProfileId, quota.value]);

  const used = usage?.usedBytes ?? 0;
  if (!enabled && used === 0) return null;

  // Empty or nonsense puts back what was shown, which `key` cannot do since the
  // value has not changed. The backend reads the same rule.
  const commit = (input: HTMLInputElement) => {
    const next = Number(input.value);
    if (!Number.isSafeInteger(next) || next < 1) {
      input.value = String(quota.value);
    } else if (next !== quota.value) {
      void quota.setValue(next);
    }
  };

  return (
    <div
      className="settings-row flex items-center justify-between"
      data-podcast-quota
    >
      <div className="flex min-w-0 items-center space-x-4">
        <HardDrive
          size={16}
          className="settings-row-icon shrink-0"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.podcasts.downloadQuota")}
          </div>
          <div className="text-xs settings-description">
            {t("settings.podcasts.downloadQuotaHint", {
              used: formatBytes(used, i18n.language),
            })}
          </div>
        </div>
      </div>
      <label className="flex shrink-0 items-center gap-2 text-sm text-zinc-700 dark:text-zinc-200">
        <input
          key={quota.value}
          type="number"
          min={1}
          step={1}
          disabled={!quota.ready}
          defaultValue={quota.value}
          aria-label={t("settings.podcasts.downloadQuota")}
          onBlur={(event) => commit(event.target)}
          onKeyDown={(event) => {
            if (event.key === "Enter") commit(event.currentTarget);
          }}
          className="w-20 rounded-lg bg-zinc-100 px-2 py-1 text-zinc-900 dark:bg-zinc-800 dark:text-white"
        />
        <span aria-hidden="true">
          {new Intl.NumberFormat(i18n.language, {
            style: "unit",
            unit: "gigabyte",
            unitDisplay: "short",
          })
            .formatToParts(1)
            .find((part) => part.type === "unit")?.value ?? "GB"}
        </span>
      </label>
    </div>
  );
}
