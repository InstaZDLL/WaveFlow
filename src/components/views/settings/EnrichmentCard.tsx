import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { ListChecks, Play, Square, Wand2 } from "lucide-react";

import {
  cancelEnrichment,
  ENRICHMENT_CHANGED_EVENT,
  getEnrichmentStatus,
  runEnrichment,
  type EnrichmentStats,
} from "../../../lib/tauri/enrichment";
import {
  useEnrichmentToggle,
  type EnrichmentSettingKey,
} from "../../../hooks/useEnrichmentSettings";
import { useProfile } from "../../../hooks/useProfile";
import { useTasks } from "../../../hooks/useTasks";
import { EnrichmentReviewModal } from "../../common/EnrichmentReviewModal";
import { ToggleSwitch } from "./ToggleSwitch";

const FIELDS: ReadonlyArray<{ key: EnrichmentSettingKey; labelKey: string }> = [
  {
    key: "enrichment.fill_genres",
    labelKey: "settings.enrichment.fields.genres",
  },
  {
    key: "enrichment.fill_dates",
    labelKey: "settings.enrichment.fields.dates",
  },
  {
    key: "enrichment.fill_covers",
    labelKey: "settings.enrichment.fields.covers",
  },
  {
    key: "enrichment.fill_credits",
    labelKey: "settings.enrichment.fields.credits",
  },
];

function FieldToggle({
  settingKey,
  labelKey,
  disabled,
}: {
  settingKey: EnrichmentSettingKey;
  labelKey: string;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  const toggle = useEnrichmentToggle(settingKey);
  return (
    <div className="flex items-center justify-between gap-4 py-1.5">
      <span className="text-sm text-zinc-800 dark:text-zinc-200">
        {t(labelKey)}
      </span>
      <ToggleSwitch
        enabled={toggle.value}
        onToggle={() => void toggle.setValue(!toggle.value)}
        label={t(labelKey)}
        disabled={disabled || !toggle.ready}
      />
    </div>
  );
}

/**
 * Settings → Library → Enrichment: the engine that completes the library
 * from MusicBrainz — its switch, what it may fill, a pass on demand, and
 * the way into the review of what it was not sure about.
 */
export function EnrichmentCard() {
  const { t } = useTranslation();
  const enabled = useEnrichmentToggle("enrichment.enabled");
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  const tasks = useTasks();
  const task = tasks.find((entry) => entry.kind === "enrichment");
  const [stats, setStats] = useState<EnrichmentStats | null>(null);
  const [reviewOpen, setReviewOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Which read is current: a profile switch keeps the card mounted, and
  // an answer about the previous profile must not land after the new one.
  const readSeq = useRef(0);
  const refresh = useCallback(() => {
    const seq = ++readSeq.current;
    getEnrichmentStatus()
      .then((status) => {
        if (seq === readSeq.current) setStats(status.stats);
      })
      .catch((err) => console.warn("[EnrichmentCard] status failed", err));
  }, []);

  useEffect(() => {
    // Subscribe first, then read: the first read starts once the
    // listener is live, so a pass that lands between the two is heard
    // rather than lost (see the "Events" invariant). Re-run on a profile
    // switch, whose counts are another database's.
    let off: (() => void) | undefined;
    let cancelled = false;
    listen(ENRICHMENT_CHANGED_EVENT, refresh)
      .then((unlisten) => {
        if (cancelled) {
          unlisten();
          return;
        }
        off = unlisten;
        refresh();
      })
      .catch((err) => {
        console.warn("[EnrichmentCard] listen failed", err);
        if (!cancelled) refresh();
      });
    return () => {
      cancelled = true;
      off?.();
    };
  }, [refresh, activeProfileId]);

  const onToggleEnabled = async () => {
    const next = !enabled.value;
    setError(null);
    try {
      await enabled.setValue(next);
      // Turning it on starts a pass straight away rather than waiting
      // for the next scan.
      if (next) await runEnrichment();
      else await cancelEnrichment();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const onRun = () => {
    setError(null);
    runEnrichment().catch((err) =>
      setError(err instanceof Error ? err.message : String(err)),
    );
  };

  const toReview = (stats?.albums_review ?? 0) + (stats?.artists_review ?? 0);

  return (
    <div className="settings-block">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4">
          <Wand2 size={16} className="settings-row-icon" aria-hidden="true" />
          <div>
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.enrichment.title")}
            </div>
            <div className="text-xs settings-description">
              {t("settings.enrichment.subtitle")}
            </div>
          </div>
        </div>
        <ToggleSwitch
          enabled={enabled.value}
          onToggle={() => void onToggleEnabled()}
          label={t("settings.enrichment.title")}
          disabled={!enabled.ready}
        />
      </div>

      <div className="mt-3 ml-9 space-y-3">
        <p className="text-xs settings-description">
          {t("settings.enrichment.policy")}
        </p>

        <div>
          <div className="text-xs font-medium uppercase tracking-wide text-zinc-500 mb-1">
            {t("settings.enrichment.fieldsTitle")}
          </div>
          {FIELDS.map((field) => (
            <FieldToggle
              key={field.key}
              settingKey={field.key}
              labelKey={field.labelKey}
              disabled={!enabled.value}
            />
          ))}
        </div>

        {stats && (
          <dl className="grid grid-cols-2 sm:grid-cols-4 gap-2 text-center">
            {(
              [
                ["applied", stats.albums_applied],
                ["review", toReview],
                ["noMatch", stats.albums_no_match],
                ["artists", stats.artists_applied],
              ] as const
            ).map(([key, count]) => (
              <div
                key={key}
                className="rounded-xl border border-zinc-200 dark:border-zinc-700 px-2 py-2"
              >
                <dt className="text-[11px] text-zinc-500">
                  {t(`settings.enrichment.stats.${key}`)}
                </dt>
                <dd className="text-base font-semibold tabular-nums text-zinc-900 dark:text-white">
                  {count}
                </dd>
              </div>
            ))}
          </dl>
        )}

        {task && (
          <div>
            <div className="flex justify-between gap-2 text-[11px] text-zinc-500 mb-1">
              <span className="truncate">{task.detail ?? ""}</span>
              <span className="tabular-nums shrink-0">
                {task.total > 0 ? `${task.current} / ${task.total}` : ""}
              </span>
            </div>
            <div className="h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
              <div
                className={
                  task.total > 0
                    ? "h-full bg-emerald-500 transition-all duration-200"
                    : "h-full w-1/3 bg-emerald-500 opacity-70 motion-safe:animate-pulse"
                }
                style={
                  task.total > 0
                    ? {
                        width: `${Math.round((task.current / task.total) * 100)}%`,
                      }
                    : undefined
                }
              />
            </div>
          </div>
        )}

        <div className="flex flex-wrap gap-2">
          {task ? (
            <button
              type="button"
              onClick={() => void cancelEnrichment()}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-rose-200 bg-rose-50 text-sm font-medium text-rose-700 hover:bg-rose-100 dark:border-rose-900/40 dark:bg-rose-950/30 dark:text-rose-300 dark:hover:bg-rose-950/50 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-rose-500"
            >
              <Square size={14} aria-hidden="true" />
              <span>{t("settings.enrichment.stop")}</span>
            </button>
          ) : (
            <button
              type="button"
              onClick={onRun}
              disabled={!enabled.value}
              className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <Play size={14} aria-hidden="true" />
              <span>{t("settings.enrichment.run")}</span>
            </button>
          )}
          <button
            type="button"
            onClick={() => setReviewOpen(true)}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <ListChecks size={14} aria-hidden="true" />
            <span>{t("settings.enrichment.review", { count: toReview })}</span>
          </button>
        </div>

        {error && (
          <p role="alert" className="text-xs text-rose-600 dark:text-rose-400">
            {error}
          </p>
        )}
      </div>

      <EnrichmentReviewModal
        isOpen={reviewOpen}
        onClose={() => setReviewOpen(false)}
      />
    </div>
  );
}
