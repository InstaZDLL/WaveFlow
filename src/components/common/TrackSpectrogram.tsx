import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AudioWaveform, Loader2, X } from "lucide-react";
import { useTasks } from "../../hooks/useTasks";
import {
  cancelTrackSpectrogram,
  trackSpectrogram,
  type TrackSpectrogram as Spectrogram,
} from "../../lib/tauri/analysis";

/** Columns asked of the backend: the chart's width at 1.5× scale. */
const WIDTH = 900;

/** Where a lossy encoder usually cuts: around 16 kHz at 128 kbps, 19 at
 *  192, 20 at 320. Drawn as guides so the eye has something to read the
 *  wall against. */
const GUIDES_HZ = [16_000, 19_000, 20_000];

/** Height of the chart in CSS pixels (`h-44`), and the least room one
 *  axis label needs: at 44.1 kHz, 19 and 20 kHz stand 8 px apart. */
const CHART_PX = 176;
const LABEL_PX = 11;

interface AxisLabel {
  key: string;
  text: string;
  /** Where the line it names stands, in px from the top. */
  y: number;
  cutoff: boolean;
}

/**
 * Spread the labels so none overlaps the one above: each is pushed down
 * to at least `LABEL_PX` under its neighbour, then the run is pulled back
 * up if it would leave the chart. The lines themselves stay where they
 * are; only their names move, by a few pixels.
 */
function layoutLabels(labels: AxisLabel[]): (AxisLabel & { top: number })[] {
  const sorted = [...labels].sort((a, b) => a.y - b.y);
  const tops: number[] = [];
  for (const [i, label] of sorted.entries()) {
    tops.push(i === 0 ? label.y : Math.max(label.y, tops[i - 1] + LABEL_PX));
  }
  for (let i = tops.length - 1; i >= 0; i--) {
    const limit = i === tops.length - 1 ? CHART_PX : tops[i + 1] - LABEL_PX;
    tops[i] = Math.min(tops[i], limit);
  }
  return sorted.map((label, i) => ({ ...label, top: tops[i] }));
}

/** Dark to bright through violet, red and yellow, so a 20 dB fall reads as
 *  a change of hue rather than only of brightness. */
const STOPS: [number, [number, number, number]][] = [
  [0, [0, 0, 4]],
  [0.25, [40, 11, 84]],
  [0.5, [159, 42, 99]],
  [0.7, [229, 92, 48]],
  [0.87, [249, 184, 14]],
  [1, [252, 255, 164]],
];

function palette(): Uint8ClampedArray {
  const lut = new Uint8ClampedArray(256 * 3);
  for (let i = 0; i < 256; i++) {
    const x = i / 255;
    let k = 0;
    while (k < STOPS.length - 2 && x > STOPS[k + 1][0]) k++;
    const [x0, c0] = STOPS[k];
    const [x1, c1] = STOPS[k + 1];
    const f = (x - x0) / (x1 - x0);
    for (let c = 0; c < 3; c++) lut[i * 3 + c] = c0[c] + (c1[c] - c0[c]) * f;
  }
  return lut;
}

function formatTime(seconds: number): string {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/**
 * The spectrum of a library track over time, drawn under the
 * fake-lossless verdict so the user can see where the highs stop.
 *
 * Asked for on demand — it costs a full decode — and stopped when the
 * dialog closes or moves to another track. The decode reports into the
 * task registry, so the bar here is the measured share of samples
 * decoded, or an honest spinner when the file gives no length.
 */
export function TrackSpectrogramPanel({ trackId }: { trackId: number }) {
  const { t, i18n } = useTranslation();
  const [picture, setPicture] = useState<Spectrogram | null>(null);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const tasks = useTasks();
  const task = running ? tasks.find((x) => x.kind === "spectrogram") : null;

  // Leaving the dialog, or the track, stops a decode still running.
  useEffect(
    () => () => {
      void cancelTrackSpectrogram().catch(() => {});
    },
    [trackId],
  );

  const start = useCallback(async () => {
    setRunning(true);
    setError(null);
    try {
      const result = await trackSpectrogram(trackId, WIDTH);
      if (result) setPicture(result);
    } catch (err) {
      setError(String(err));
    } finally {
      setRunning(false);
    }
  }, [trackId]);

  const stop = useCallback(() => {
    void cancelTrackSpectrogram().catch(() => {});
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !picture) return;
    const { columns, rows } = picture;
    canvas.width = columns;
    canvas.height = rows;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const bytes = Uint8Array.from(atob(picture.cells), (c) => c.charCodeAt(0));
    const lut = palette();
    const image = ctx.createImageData(columns, rows);
    for (let x = 0; x < columns; x++) {
      for (let r = 0; r < rows; r++) {
        const v = bytes[x * rows + r];
        // Row 0 is 0 Hz, drawn at the bottom.
        const o = ((rows - 1 - r) * columns + x) * 4;
        image.data[o] = lut[v * 3];
        image.data[o + 1] = lut[v * 3 + 1];
        image.data[o + 2] = lut[v * 3 + 2];
        image.data[o + 3] = 255;
      }
    }
    ctx.putImageData(image, 0, 0);
  }, [picture]);

  const khz = useMemo(
    () =>
      new Intl.NumberFormat(i18n.resolvedLanguage, {
        maximumFractionDigits: 2,
      }),
    [i18n.resolvedLanguage],
  );

  const nyquist = picture ? picture.sampleRate / 2 : 0;
  const cutoff = picture?.cutoffHz ?? null;
  const marks = picture
    ? [
        ...GUIDES_HZ.filter((hz) => hz < nyquist - 500).map((hz) => ({
          hz,
          kind: "guide" as const,
        })),
        ...(cutoff != null ? [{ hz: cutoff, kind: "cutoff" as const }] : []),
      ]
    : [];
  const yOf = (hz: number) => (1 - hz / nyquist) * CHART_PX;
  const labels = picture
    ? layoutLabels([
        {
          key: "nyquist",
          text: t("trackProperties.spectrogram.axisKhz", {
            value: khz.format(nyquist / 1000),
          }),
          y: 0,
          cutoff: false,
        },
        // A guide the measured wall stands next to keeps its line but
        // lends its label to the wall, which is the one worth reading.
        ...marks
          .filter(
            (m) =>
              m.kind === "cutoff" ||
              cutoff == null ||
              Math.abs(m.hz - cutoff) > 1_000,
          )
          .map((m) => ({
            key: `${m.kind}-${m.hz}`,
            text: t("trackProperties.spectrogram.axisKhz", {
              value: khz.format(m.hz / 1000),
            }),
            y: yOf(m.hz),
            cutoff: m.kind === "cutoff",
          })),
        { key: "zero", text: "0", y: CHART_PX, cutoff: false },
      ])
    : [];
  const duration = picture?.durationSeconds ?? 0;
  const percent =
    task && task.total > 0
      ? Math.min(100, Math.round((task.current / task.total) * 100))
      : null;

  const summary = picture
    ? picture.cutoffHz != null
      ? t("trackProperties.spectrogram.altCutoff", {
          khz: khz.format(picture.cutoffHz / 1000),
          nyquist: khz.format(nyquist / 1000),
        })
      : t("trackProperties.spectrogram.altFull", {
          nyquist: khz.format(nyquist / 1000),
        })
    : "";

  return (
    <div className="px-3 py-2.5 text-sm">
      {!picture && (
        <div className="flex items-center justify-between gap-3">
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            {t("trackProperties.spectrogram.hint")}
          </p>
          {running ? (
            <div className="flex shrink-0 items-center gap-2">
              {percent != null ? (
                <div
                  role="progressbar"
                  aria-label={t("trackProperties.spectrogram.decoding")}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-valuenow={percent}
                  className="h-1.5 w-24 overflow-hidden rounded-full bg-zinc-200 dark:bg-zinc-700"
                >
                  <div
                    className="h-full bg-emerald-500 transition-[width]"
                    style={{ width: `${percent}%` }}
                  />
                </div>
              ) : (
                <Loader2
                  size={14}
                  className="animate-spin text-zinc-400"
                  aria-label={t("trackProperties.spectrogram.decoding")}
                />
              )}
              <button
                type="button"
                onClick={stop}
                className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-[11px] font-medium text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800 dark:hover:bg-zinc-800 dark:hover:text-zinc-200 transition-colors"
              >
                <X size={12} aria-hidden="true" />
                <span>{t("trackProperties.spectrogram.stop")}</span>
              </button>
            </div>
          ) : (
            <button
              type="button"
              onClick={() => void start()}
              className="inline-flex shrink-0 items-center gap-1.5 rounded-md px-2 py-1 text-[11px] font-medium text-emerald-600 hover:bg-emerald-50 dark:text-emerald-400 dark:hover:bg-emerald-500/10 transition-colors"
            >
              <AudioWaveform size={12} aria-hidden="true" />
              <span>{t("trackProperties.spectrogram.show")}</span>
            </button>
          )}
        </div>
      )}
      {error && (
        <p
          role="alert"
          className="mt-1 text-xs text-amber-700 dark:text-amber-300"
        >
          {t("trackProperties.spectrogram.failed")}
        </p>
      )}
      {picture && (
        <figure className="m-0">
          <div className="flex gap-2">
            {/* Frequency axis: label positions follow the linear scale. */}
            <div
              className="relative h-44 w-20 shrink-0 text-[10px] tabular-nums text-zinc-500 dark:text-zinc-400"
              aria-hidden="true"
            >
              {labels.map((label) => (
                <span
                  key={label.key}
                  className={`absolute right-0 -translate-y-1/2 whitespace-nowrap ${
                    label.cutoff
                      ? "font-semibold text-emerald-600 dark:text-emerald-400"
                      : ""
                  }`}
                  style={{ top: `${label.top}px` }}
                >
                  {label.text}
                </span>
              ))}
            </div>
            <div className="relative h-44 min-w-0 flex-1 overflow-hidden rounded-md bg-black">
              <canvas
                ref={canvasRef}
                role="img"
                aria-label={summary}
                className="block h-full w-full"
              />
              {marks.map((m) => (
                <div
                  key={`line-${m.kind}-${m.hz}`}
                  aria-hidden="true"
                  className={`pointer-events-none absolute inset-x-0 border-t ${
                    m.kind === "cutoff"
                      ? "border-dashed border-emerald-400"
                      : "border-white/25"
                  }`}
                  style={{ top: `${(1 - m.hz / nyquist) * 100}%` }}
                />
              ))}
            </div>
          </div>
          <div
            className="ms-[5.5rem] mt-1 flex justify-between text-[10px] tabular-nums text-zinc-500 dark:text-zinc-400"
            aria-hidden="true"
          >
            <span>0:00</span>
            <span>{formatTime(duration)}</span>
          </div>
          <figcaption className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
            {summary} {t("trackProperties.spectrogram.guides")}
          </figcaption>
        </figure>
      )}
    </div>
  );
}
