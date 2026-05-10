import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";
import {
  playerGetEq,
  playerSetEqBand,
  playerSetEqEnabled,
  playerSetEqPreset,
  type EqPresetEntry,
} from "../../../lib/tauri/eq";

/**
 * Spotify-style 6-band equaliser card. Renders a draggable curve over
 * the band frequencies, a preset dropdown, a bypass toggle and a
 * Reset button. Backend lives in `src-tauri/src/audio/eq.rs`; this
 * component just wires the curve to the per-band atomic setters.
 *
 * Each draggable point writes through the Tauri command on every
 * pointermove (no local debounce — the backend atomic store + lazy
 * coefficient recompute makes this cheap, and per-frame updates feel
 * much more reactive than a debounced "release-only" mode).
 */
export function EqualizerCard() {
  const { t } = useTranslation();

  const [enabled, setEnabled] = useState(false);
  const [bands, setBands] = useState<number[]>([0, 0, 0, 0, 0, 0]);
  const [freqs, setFreqs] = useState<number[]>([
    60, 150, 400, 1000, 2400, 15000,
  ]);
  const [maxGain, setMaxGain] = useState(12);
  const [presets, setPresets] = useState<EqPresetEntry[]>([]);
  const [presetOpen, setPresetOpen] = useState(false);
  const presetRef = useRef<HTMLDivElement>(null);

  // Hydrate from backend at mount.
  useEffect(() => {
    playerGetEq()
      .then((snap) => {
        setEnabled(snap.enabled);
        setBands(snap.bands_db);
        setFreqs(snap.band_freqs);
        setMaxGain(snap.max_gain_db);
        setPresets(snap.presets);
      })
      .catch((err) => console.error("[EqualizerCard] hydrate failed", err));
  }, []);

  // Identify the active preset by exact-gain match (within 0.01 dB).
  // Falls back to "custom" when the user has nudged anything off a
  // preset value.
  const activePresetKey = useMemo(() => {
    const match = presets.find((p) =>
      p.gains.every((g, i) => Math.abs((bands[i] ?? 0) - g) < 0.01),
    );
    return match?.key ?? "custom";
  }, [presets, bands]);

  // Close the preset menu on outside click / Escape.
  useEffect(() => {
    if (!presetOpen) return;
    const onClick = (e: MouseEvent) => {
      if (presetRef.current && !presetRef.current.contains(e.target as Node)) {
        setPresetOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPresetOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [presetOpen]);

  const handleToggle = useCallback(() => {
    const next = !enabled;
    setEnabled(next);
    playerSetEqEnabled(next).catch((err) => {
      console.error("[EqualizerCard] toggle failed", err);
      setEnabled(!next);
    });
  }, [enabled]);

  const handlePickPreset = useCallback(
    (key: string) => {
      const preset = presets.find((p) => p.key === key);
      if (!preset) return;
      setBands(preset.gains);
      setPresetOpen(false);
      playerSetEqPreset(key).catch((err) =>
        console.error("[EqualizerCard] preset failed", err),
      );
    },
    [presets],
  );

  const handleReset = useCallback(() => {
    handlePickPreset("flat");
  }, [handlePickPreset]);

  const updateBand = useCallback((index: number, gainDb: number) => {
    const clamped = Math.max(-12, Math.min(12, gainDb));
    setBands((prev) => {
      if (Math.abs((prev[index] ?? 0) - clamped) < 0.01) return prev;
      const next = [...prev];
      next[index] = clamped;
      return next;
    });
    playerSetEqBand(index, clamped).catch((err) =>
      console.error("[EqualizerCard] set band failed", err),
    );
  }, []);

  return (
    <div className="rounded-2xl border border-zinc-200 dark:border-zinc-800 overflow-hidden">
      {/* Header row: title + master toggle */}
      <div className="flex items-center justify-between px-5 py-4 border-b border-zinc-100 dark:border-zinc-800">
        <div>
          <div className="text-sm font-medium text-zinc-900 dark:text-white">
            {t("settings.equalizer.title")}
          </div>
          <div className="text-xs text-zinc-400 mt-0.5">
            {t("settings.equalizer.subtitle")}
          </div>
        </div>
        <ToggleSwitch
          enabled={enabled}
          onToggle={handleToggle}
          label={t("settings.equalizer.title")}
        />
      </div>

      {/* Curve + presets — visually muted when bypass is off so the
          user understands the toggle is the master switch. */}
      <div
        className={`px-5 py-5 transition-opacity ${
          enabled ? "opacity-100" : "opacity-40 pointer-events-none"
        }`}
      >
        {/* Preset dropdown row */}
        <div className="flex items-center justify-between mb-4">
          <div className="flex items-center gap-3 relative" ref={presetRef}>
            <span className="text-xs uppercase tracking-widest text-zinc-500">
              {t("settings.equalizer.presets")}
            </span>
            <button
              type="button"
              onClick={() => setPresetOpen((v) => !v)}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-zinc-200 dark:border-zinc-700 hover:border-zinc-300 dark:hover:border-zinc-600 text-sm text-zinc-800 dark:text-zinc-200 transition-colors"
            >
              {t(`settings.equalizer.preset.${activePresetKey}`, {
                defaultValue:
                  activePresetKey === "custom"
                    ? t("settings.equalizer.preset.custom")
                    : activePresetKey,
              })}
              <ChevronDown size={14} className="text-zinc-400" />
            </button>
            {presetOpen && (
              <div className="absolute top-full left-22 mt-2 z-20 w-44 max-h-72 overflow-y-auto rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-800 shadow-lg">
                {presets.map((p) => (
                  <button
                    key={p.key}
                    type="button"
                    onClick={() => handlePickPreset(p.key)}
                    className={`w-full text-left px-3 py-2 text-sm transition-colors ${
                      p.key === activePresetKey
                        ? "bg-emerald-500 text-white"
                        : "hover:bg-zinc-100 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-200"
                    }`}
                  >
                    {t(`settings.equalizer.preset.${p.key}`, {
                      defaultValue: p.key,
                    })}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            type="button"
            onClick={handleReset}
            className="text-xs px-3 py-1.5 rounded-full border border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300 hover:border-zinc-300 dark:hover:border-zinc-600 transition-colors"
          >
            {t("settings.equalizer.reset")}
          </button>
        </div>

        <EqCurve
          bands={bands}
          freqs={freqs}
          maxGain={maxGain}
          onUpdateBand={updateBand}
        />
      </div>
    </div>
  );
}

interface EqCurveProps {
  bands: number[];
  freqs: number[];
  maxGain: number;
  onUpdateBand: (index: number, gainDb: number) => void;
}

/**
 * SVG-based draggable curve. The viewBox stays at a constant
 * 600 × 240 internal coordinate system; CSS scales it responsively.
 * Each band has a draggable circle anchored at its frequency column.
 * The fill polygon between adjacent points gives the Spotify-like
 * green plateau.
 */
function EqCurve({ bands, freqs, maxGain, onUpdateBand }: EqCurveProps) {
  // Aspect tuned to match Spotify's plot block (wider than tall, with
  // dedicated label gutter on the left). The container's `aspect-[7/3]`
  // CSS keeps the SVG visually identical at any width without
  // disabling preserveAspectRatio (which is what made the curve look
  // horizontally stretched in v1).
  const VB_W = 700;
  const VB_H = 320;
  const PAD_TOP = 28;
  // Bottom padding holds two stacked label rows: the frequency
  // (always visible) and the live dB readout below it.
  const PAD_BOTTOM = 56;
  // Left gutter holds the +12dB / -12dB labels; right gutter is just
  // breathing room so the rightmost dot doesn't kiss the SVG edge.
  const PAD_LEFT = 56;
  const PAD_RIGHT = 24;
  const usableW = VB_W - PAD_LEFT - PAD_RIGHT;
  const usableH = VB_H - PAD_TOP - PAD_BOTTOM;
  const midY = PAD_TOP + usableH / 2;

  const svgRef = useRef<SVGSVGElement | null>(null);
  const draggingRef = useRef<number | null>(null);

  // X position for band index — even spacing across the plot area.
  const xFor = useCallback(
    (i: number) => PAD_LEFT + (i / (bands.length - 1)) * usableW,
    [bands.length, usableW],
  );

  // Y position for a gain (-maxGain → maxGain mapped to bottom → top).
  const yFor = useCallback(
    (gain: number) => {
      const norm = (gain + maxGain) / (2 * maxGain); // 0..1
      return PAD_TOP + (1 - norm) * usableH;
    },
    [maxGain, usableH],
  );

  // Inverse: pixel Y inside the SVG → gain.
  const gainForPixel = useCallback(
    (svgY: number) => {
      const norm = 1 - (svgY - PAD_TOP) / usableH;
      const gain = norm * (2 * maxGain) - maxGain;
      return Math.max(-maxGain, Math.min(maxGain, gain));
    },
    [maxGain, usableH],
  );

  const eventToSvgY = useCallback(
    (e: PointerEvent | ReactPointerEvent) => {
      const svg = svgRef.current;
      if (!svg) return midY;
      const pt = svg.createSVGPoint();
      pt.x = (e as PointerEvent).clientX;
      pt.y = (e as PointerEvent).clientY;
      const ctm = svg.getScreenCTM();
      if (!ctm) return midY;
      const local = pt.matrixTransform(ctm.inverse());
      return local.y;
    },
    [midY],
  );

  const handlePointerDown = useCallback(
    (index: number) => (e: ReactPointerEvent<SVGCircleElement>) => {
      e.preventDefault();
      (e.target as SVGCircleElement).setPointerCapture(e.pointerId);
      draggingRef.current = index;
    },
    [],
  );

  const handlePointerMove = useCallback(
    (e: ReactPointerEvent<SVGCircleElement>) => {
      const idx = draggingRef.current;
      if (idx == null) return;
      const y = eventToSvgY(e.nativeEvent);
      onUpdateBand(idx, gainForPixel(y));
    },
    [eventToSvgY, gainForPixel, onUpdateBand],
  );

  const handlePointerUp = useCallback(
    (e: ReactPointerEvent<SVGCircleElement>) => {
      draggingRef.current = null;
      try {
        (e.target as SVGCircleElement).releasePointerCapture(e.pointerId);
      } catch {
        // PointerCapture may already be released — ignore.
      }
    },
    [],
  );

  // Smooth curve through the bands using a Catmull-Rom spline
  // converted to cubic Bezier control points. This is what gives
  // Spotify's EQ its characteristic "bumps between bands" instead of
  // straight zig-zag segments. Endpoints are duplicated so the spline
  // doesn't rely on phantom neighbours past the edge.
  const pts = bands.map((g, i) => ({ x: xFor(i), y: yFor(g) }));
  const splinePath = (() => {
    if (pts.length < 2) return "";
    const get = (i: number) => pts[Math.max(0, Math.min(pts.length - 1, i))];
    let d = `M ${pts[0].x} ${pts[0].y}`;
    for (let i = 0; i < pts.length - 1; i++) {
      const p0 = get(i - 1);
      const p1 = get(i);
      const p2 = get(i + 1);
      const p3 = get(i + 2);
      // Catmull-Rom → Bezier: tension 0.5 (the /6 standard form).
      const c1x = p1.x + (p2.x - p0.x) / 6;
      const c1y = p1.y + (p2.y - p0.y) / 6;
      const c2x = p2.x - (p3.x - p1.x) / 6;
      const c2y = p2.y - (p3.y - p1.y) / 6;
      d += ` C ${c1x} ${c1y}, ${c2x} ${c2y}, ${p2.x} ${p2.y}`;
    }
    return d;
  })();

  // Same path but closed down to the mid-axis to render the filled
  // polygon. Reusing the spline guarantees stroke + fill follow the
  // exact same curve.
  const fillPath =
    `M ${pts[0].x} ${midY} L ${pts[0].x} ${pts[0].y}` +
    splinePath.slice(splinePath.indexOf("C") - 1) +
    ` L ${pts[pts.length - 1].x} ${midY} Z`;

  return (
    <div className="rounded-xl bg-zinc-50 dark:bg-zinc-900/40 p-3">
      <div className="w-full" style={{ aspectRatio: `${VB_W} / ${VB_H}` }}>
        <svg
          ref={svgRef}
          viewBox={`0 0 ${VB_W} ${VB_H}`}
          className="w-full h-full touch-none"
        >
          {/* dB scale labels — anchored end so they sit just left of the
            plot's vertical edge, mirroring Spotify's left gutter. */}
          <text
            x={PAD_LEFT - 12}
            y={PAD_TOP + 6}
            fontSize={13}
            textAnchor="end"
            fill="currentColor"
            className="text-zinc-400 select-none"
          >
            +{maxGain}dB
          </text>
          <text
            x={PAD_LEFT - 12}
            y={PAD_TOP + usableH + 4}
            fontSize={13}
            textAnchor="end"
            fill="currentColor"
            className="text-zinc-400 select-none"
          >
            -{maxGain}dB
          </text>

          {/* Mid-axis line */}
          <line
            x1={PAD_LEFT}
            y1={midY}
            x2={PAD_LEFT + usableW}
            y2={midY}
            stroke="currentColor"
            strokeWidth={1}
            className="text-zinc-300 dark:text-zinc-700"
          />

          {/* Vertical guides at each band frequency */}
          {bands.map((_, i) => (
            <line
              key={`g-${i}`}
              x1={xFor(i)}
              y1={PAD_TOP}
              x2={xFor(i)}
              y2={PAD_TOP + usableH}
              stroke="currentColor"
              strokeWidth={1}
              className="text-zinc-200/70 dark:text-zinc-700/40"
            />
          ))}

          {/* Filled gradient under the curve */}
          <defs>
            <linearGradient id="eqFill" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stopColor="rgb(16 185 129)" stopOpacity="0.7" />
              <stop
                offset="100%"
                stopColor="rgb(16 185 129)"
                stopOpacity="0.08"
              />
            </linearGradient>
          </defs>
          <path d={fillPath} fill="url(#eqFill)" />

          {/* Smooth curve stroke */}
          <path
            d={splinePath}
            fill="none"
            stroke="rgb(16 185 129)"
            strokeWidth={2.5}
            strokeLinejoin="round"
            strokeLinecap="round"
          />

          {/* Draggable points — slightly larger than the v1 to match
            Spotify's tap target size. */}
          {bands.map((g, i) => (
            <circle
              key={`p-${i}`}
              cx={xFor(i)}
              cy={yFor(g)}
              r={9}
              fill="white"
              stroke="rgb(16 185 129)"
              strokeWidth={2.5}
              className="cursor-grab active:cursor-grabbing drop-shadow-sm"
              onPointerDown={handlePointerDown(i)}
              onPointerMove={handlePointerMove}
              onPointerUp={handlePointerUp}
              onPointerCancel={handlePointerUp}
            />
          ))}

          {/* X-axis frequency labels */}
          {freqs.map((f, i) => (
            <text
              key={`f-${i}`}
              x={xFor(i)}
              y={VB_H - 30}
              fontSize={14}
              fontWeight={600}
              textAnchor="middle"
              fill="currentColor"
              className="text-zinc-500 dark:text-zinc-400 select-none"
            >
              {formatFreq(f)}
            </text>
          ))}

          {/* Live dB readout per band — Logitech-G-style, useful while
            dragging because the curve alone doesn't tell you exactly
            where the point sits. */}
          {bands.map((g, i) => (
            <text
              key={`v-${i}`}
              x={xFor(i)}
              y={VB_H - 12}
              fontSize={12}
              textAnchor="middle"
              fill="currentColor"
              className={`select-none tabular-nums ${
                Math.abs(g) < 0.05
                  ? "text-zinc-400 dark:text-zinc-500"
                  : "text-emerald-600 dark:text-emerald-400 font-semibold"
              }`}
            >
              {formatGain(g)}
            </text>
          ))}
        </svg>
      </div>
    </div>
  );
}

/** Local mirror of SettingsView's ToggleSwitch — kept inline to
 *  avoid coupling this card to the parent view's private helper. */
function ToggleSwitch({
  enabled,
  onToggle,
  label,
}: {
  enabled: boolean;
  onToggle: () => void;
  label: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      role="switch"
      aria-checked={enabled}
      aria-label={label}
      className={`relative w-12 h-7 rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 dark:focus-visible:ring-offset-zinc-900 ${
        enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-600"
      }`}
    >
      <div
        className={`absolute top-0.5 w-6 h-6 rounded-full bg-white shadow-sm transition-transform ${
          enabled ? "left-[calc(100%-1.625rem)]" : "left-0.5"
        }`}
      />
    </button>
  );
}

/** Format the per-band live readout. Snaps near-zero to "0 dB" so
 *  the axis row stays readable when the band is centred. */
function formatGain(db: number): string {
  if (Math.abs(db) < 0.05) return "0 dB";
  const sign = db > 0 ? "+" : "";
  return `${sign}${db.toFixed(1)} dB`;
}

function formatFreq(hz: number): string {
  if (hz >= 1000) {
    const k = hz / 1000;
    return Number.isInteger(k) ? `${k}kHz` : `${k.toFixed(1)}kHz`;
  }
  return `${hz}Hz`;
}
