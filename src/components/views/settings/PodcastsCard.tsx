import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Podcast } from "lucide-react";

import { usePodcastsEnabled } from "../../../hooks/usePodcastsEnabled";
import { useProfile } from "../../../hooks/useProfile";
import { openPodcastWarning, openView } from "../../../lib/discovery";
import {
  PODCAST_UPDATED,
  podcastListChannels,
} from "../../../lib/tauri/podcast";
import { ToggleSwitch } from "./ToggleSwitch";

/**
 * Settings → Library → Podcasts: the switch. Turning on goes through the warning,
 * which this row only opens. Turning off asks for nothing, and says what stays.
 */
export function PodcastsCard() {
  const { t } = useTranslation();
  const { enabled, ready, turnOff } = usePodcastsEnabled();
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  const [channels, setChannels] = useState(0);

  // Which read is current: a profile switch keeps the card mounted.
  const readSeq = useRef(0);
  const refresh = useCallback(() => {
    const seq = ++readSeq.current;
    podcastListChannels()
      .then((rows) => {
        if (seq === readSeq.current) setChannels(rows.length);
      })
      .catch((err) => {
        console.warn("[PodcastsCard] listing the channels failed", err);
        if (seq === readSeq.current) setChannels(0);
      });
  }, []);

  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    listen(PODCAST_UPDATED, refresh)
      .then((unlisten) => {
        if (cancelled) unlisten();
        else off = unlisten;
      })
      .catch((err) => console.warn("[PodcastsCard] listen failed", err))
      .finally(() => {
        if (!cancelled) refresh();
      });
    return () => {
      cancelled = true;
      readSeq.current += 1;
      off?.();
    };
  }, [refresh, activeProfileId]);

  return (
    <div className="settings-block" data-podcasts-card>
      <div className="flex items-center justify-between gap-4">
        <div className="flex min-w-0 items-center space-x-4">
          <Podcast
            size={16}
            className="settings-row-icon shrink-0"
            aria-hidden="true"
          />
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.podcasts.title")}
            </div>
            <div className="text-xs settings-description">
              {t(
                enabled
                  ? "settings.podcasts.subtitleOn"
                  : "settings.podcasts.subtitleOff",
              )}
            </div>
          </div>
        </div>
        <ToggleSwitch
          enabled={enabled}
          onToggle={() => {
            if (enabled) void turnOff();
            else openPodcastWarning();
          }}
          label={t("settings.podcasts.title")}
          disabled={!ready}
        />
      </div>
      {ready && !enabled && (
        <p
          role="status"
          data-podcasts-stays
          className="mt-3 ps-8 text-xs settings-description"
        >
          {t("settings.podcasts.stays")}
          {channels > 0 && (
            <> {t("settings.podcasts.staysCount", { n: channels })}</>
          )}
        </p>
      )}
      {(enabled || channels > 0) && (
        <button
          type="button"
          onClick={() => openView({ view: "podcasts" })}
          className="mt-3 ms-8 rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
        >
          {t(enabled ? "settings.podcasts.open" : "settings.podcasts.seeStays")}
        </button>
      )}
    </div>
  );
}
