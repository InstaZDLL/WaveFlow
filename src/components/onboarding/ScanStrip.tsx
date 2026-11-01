import { useTranslation } from "react-i18next";
import { AlertTriangle, Check } from "lucide-react";
import type { OnboardingScan } from "./useOnboardingScan";

/**
 * The first scan, kept in sight for the whole journey: a line of text
 * and a thin bar in the header, never a screen to wait on. Draws
 * nothing when no scan was started (the folder was skipped, or the
 * journey runs again over a library) or on a screen that hides it; its
 * status region stays mounted all the same, or an outcome reached there
 * would arrive with the region and go unannounced.
 */
export function ScanStrip({
  scan,
  visible,
}: {
  scan: OnboardingScan;
  visible: boolean;
}) {
  const { t, i18n } = useTranslation();
  const format = (n: number) => n.toLocaleString(i18n.language);

  let label = "";
  let ratio: number | null = null;
  if (scan.kind === "preparing") {
    label = t("onboarding.scan.preparing");
  } else if (scan.kind === "running") {
    label =
      scan.total > 0
        ? t("onboarding.scan.running", {
            current: format(scan.current),
            total: format(scan.total),
          })
        : t("onboarding.scan.preparing");
    ratio = scan.total > 0 ? Math.min(1, scan.current / scan.total) : null;
  } else if (scan.kind === "done") {
    label = t("onboarding.scan.done", { formatted: format(scan.added) });
    ratio = 1;
  } else if (scan.kind === "stopped") {
    label = t("onboarding.scan.stopped", { formatted: format(scan.added) });
    ratio = 1;
  } else if (scan.kind === "error") {
    label = t("onboarding.scan.failed");
  }

  // Only the outcome is announced: the count ticks all along the journey,
  // and a live region around it would read every tick aloud.
  const settled =
    scan.kind === "done" || scan.kind === "stopped" || scan.kind === "error";

  return (
    <>
      <span className="sr-only" role="status">
        {settled ? label : ""}
      </span>
      {visible && scan.kind !== "idle" && (
        <div
          className="w-56 shrink-0"
          title={scan.kind === "error" ? scan.message : undefined}
        >
          <div className="flex items-center gap-1.5 text-[11px] font-medium text-zinc-500 dark:text-zinc-400">
            {scan.kind === "done" && (
              <Check size={12} className="shrink-0 text-emerald-500" />
            )}
            {scan.kind === "error" && (
              <AlertTriangle size={12} className="shrink-0 text-amber-500" />
            )}
            <span className="truncate">{label}</span>
          </div>
          {scan.kind !== "error" && (
            <div className="onboarding-dot mt-1.5 h-1 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-800">
              <div
                className="onboarding-scan__bar h-full w-full rounded-full bg-emerald-500"
                data-indeterminate={ratio == null || undefined}
                style={
                  ratio == null ? undefined : { transform: `scaleX(${ratio})` }
                }
              />
            </div>
          )}
        </div>
      )}
    </>
  );
}
