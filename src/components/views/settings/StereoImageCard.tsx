import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import { MoveHorizontal, RotateCcw, SlidersHorizontal } from "lucide-react";

import { useProfile } from "../../../hooks/useProfile";
import { SETTINGS } from "../../../lib/settingsRegistry";
import {
  BALANCE_LIMIT,
  playerGetAudioSettings,
  playerSetBalance,
  playerSetStereoWidth,
  STEREO_WIDTH_MAX,
} from "../../../lib/tauri/player";

type Knob = "balance" | "width";

const WRITERS: Record<Knob, (value: number) => Promise<void>> = {
  balance: playerSetBalance,
  width: playerSetStereoWidth,
};

const DEFAULTS: Record<Knob, number> = {
  balance: SETTINGS["audio.balance"].defaultValue,
  width: SETTINGS["audio.stereo_width"].defaultValue,
};

/** The sliders fire on every pixel of drag; each write is a database row
 *  before it reaches the engine, so the round trip waits for a pause. */
const WRITE_DELAY_MS = 150;

/**
 * Settings → Playback and audio: the left / right balance and the stereo
 * width, two sliders with a button each that puts them back on their
 * neutral value (a double-click on the slider does the same).
 *
 * The engine saves a value before it applies it, and refuses it if the
 * save fails; the card then reads back what the engine still holds
 * instead of keeping the slider where the user left it. Writes of one
 * slider are chained, so two quick moves cannot land out of order, and
 * a write scheduled under one profile is dropped once another is active.
 */
export function StereoImageCard() {
  const { t } = useTranslation();
  const { activeProfile } = useProfile();
  const [values, setValues] = useState<Record<Knob, number>>(DEFAULTS);
  const profileIdRef = useRef(activeProfile?.id);
  const pending = useRef<Record<Knob, number | null>>({
    balance: null,
    width: null,
  });
  // The profile each pending value was set under: a write lands in that
  // profile or nowhere, never in the one active when its timer fires.
  const pendingProfile = useRef<Record<Knob, number | undefined>>({
    balance: undefined,
    width: undefined,
  });
  const timers = useRef<Record<Knob, number | null>>({
    balance: null,
    width: null,
  });
  const chain = useRef<Promise<void>>(Promise.resolve());
  // Writes of each slider scheduled on the chain and not yet settled.
  const queued = useRef<Record<Knob, number>>({ balance: 0, width: 0 });
  // Sliders moved since the profile's values were read: a read must not
  // paint over them. Cleared for a new profile, and for one slider once a
  // refused write of it is the last one it has in flight.
  const touched = useRef<Set<Knob>>(new Set());
  const hydratedProfile = useRef<number | undefined>(undefined);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    profileIdRef.current = activeProfile?.id;
  }, [activeProfile?.id]);

  useEffect(() => {
    let stale = false;
    if (hydratedProfile.current !== activeProfile?.id) {
      hydratedProfile.current = activeProfile?.id;
      touched.current = new Set();
      // A move made under the previous profile is not this one's to save.
      for (const knob of ["balance", "width"] as const) {
        const timer = timers.current[knob];
        if (timer != null) window.clearTimeout(timer);
        timers.current[knob] = null;
        pending.current[knob] = null;
      }
    }
    playerGetAudioSettings()
      .then((s) => {
        if (stale) return;
        setValues((prev) => ({
          balance: touched.current.has("balance") ? prev.balance : s.balance,
          width: touched.current.has("width") ? prev.width : s.stereo_width,
        }));
      })
      .catch((err) =>
        console.error("[StereoImageCard] audio settings load failed", err),
      );
    return () => {
      stale = true;
    };
  }, [activeProfile?.id, reload]);

  const flush = useCallback((knob: Knob) => {
    timers.current[knob] = null;
    const value = pending.current[knob];
    pending.current[knob] = null;
    if (value == null) return;
    const profileId = pendingProfile.current[knob];
    queued.current[knob] += 1;
    chain.current = chain.current.then(async () => {
      try {
        if (profileIdRef.current !== profileId) return;
        await WRITERS[knob](value);
      } catch (err) {
        console.error(`[StereoImageCard] set ${knob} failed`, err);
        // Not saved, so not applied: show what the engine still holds —
        // unless a newer move of this slider is still on its way, which
        // will settle the engine and the slider on its own value.
        if (
          profileIdRef.current === profileId &&
          queued.current[knob] === 1 &&
          timers.current[knob] == null
        ) {
          touched.current.delete(knob);
          setReload((n) => n + 1);
        }
      } finally {
        queued.current[knob] -= 1;
      }
    });
  }, []);

  const change = useCallback(
    (knob: Knob, value: number, immediate = false) => {
      touched.current.add(knob);
      setValues((prev) => ({ ...prev, [knob]: value }));
      pending.current[knob] = value;
      pendingProfile.current[knob] = profileIdRef.current;
      const timer = timers.current[knob];
      if (timer != null) window.clearTimeout(timer);
      if (immediate) {
        flush(knob);
      } else {
        timers.current[knob] = window.setTimeout(
          () => flush(knob),
          WRITE_DELAY_MS,
        );
      }
    },
    [flush],
  );

  // Leaving the page mid-drag still saves where the slider stopped.
  useEffect(
    () => () => {
      for (const knob of ["balance", "width"] as const) {
        const timer = timers.current[knob];
        if (timer != null) {
          window.clearTimeout(timer);
          flush(knob);
        }
      }
    },
    [flush],
  );

  const balanceText =
    values.balance === 0
      ? t("settings.stereoImage.balance.center")
      : t(
          values.balance < 0
            ? "settings.stereoImage.balance.left"
            : "settings.stereoImage.balance.right",
          { value: Math.abs(values.balance) },
        );
  const widthText =
    values.width === 0
      ? t("settings.stereoImage.width.mono")
      : t("settings.stereoImage.width.value", { value: values.width });

  return (
    <>
      <StereoSlider
        id="stereo-balance"
        icon={<SlidersHorizontal size={20} aria-hidden="true" />}
        title={t("settings.stereoImage.balance.title")}
        subtitle={t("settings.stereoImage.balance.subtitle")}
        min={-BALANCE_LIMIT}
        max={BALANCE_LIMIT}
        value={values.balance}
        neutral={DEFAULTS.balance}
        valueText={balanceText}
        resetLabel={t("settings.stereoImage.balance.reset")}
        onChange={(v, immediate) => change("balance", v, immediate)}
      />
      <StereoSlider
        id="stereo-width"
        icon={<MoveHorizontal size={20} aria-hidden="true" />}
        title={t("settings.stereoImage.width.title")}
        subtitle={t("settings.stereoImage.width.subtitle")}
        min={0}
        max={STEREO_WIDTH_MAX}
        value={values.width}
        neutral={DEFAULTS.width}
        valueText={widthText}
        resetLabel={t("settings.stereoImage.width.reset")}
        onChange={(v, immediate) => change("width", v, immediate)}
      />
    </>
  );
}

interface StereoSliderProps {
  id: string;
  icon: ReactNode;
  title: string;
  subtitle: string;
  min: number;
  max: number;
  value: number;
  neutral: number;
  valueText: string;
  resetLabel: string;
  onChange: (value: number, immediate: boolean) => void;
}

function StereoSlider({
  id,
  icon,
  title,
  subtitle,
  min,
  max,
  value,
  neutral,
  valueText,
  resetLabel,
  onChange,
}: StereoSliderProps) {
  const atNeutral = value === neutral;
  return (
    <div className="settings-block py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-center justify-between gap-4">
        <div className="flex items-center space-x-4 min-w-0">
          <span className="text-zinc-400 shrink-0">{icon}</span>
          <div className="min-w-0">
            <label
              htmlFor={id}
              className="text-sm font-medium text-zinc-900 dark:text-white"
            >
              {title}
            </label>
            <div className="text-xs settings-description">{subtitle}</div>
          </div>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <input
            id={id}
            type="range"
            min={min}
            max={max}
            step={1}
            value={value}
            aria-valuetext={valueText}
            onChange={(e) => onChange(Number(e.target.value), false)}
            onDoubleClick={() => onChange(neutral, true)}
            className="w-36 h-1.5 rounded-full appearance-none bg-zinc-200 dark:bg-zinc-700 accent-emerald-500 cursor-pointer"
          />
          <span
            className="text-sm font-medium text-zinc-500 w-16 text-right tabular-nums"
            aria-hidden="true"
          >
            {valueText}
          </span>
          <button
            type="button"
            onClick={() => {
              onChange(neutral, true);
              // The button disables itself; keep the focus on the row.
              document.getElementById(id)?.focus();
            }}
            disabled={atNeutral}
            aria-label={resetLabel}
            title={resetLabel}
            className="p-1.5 rounded-lg text-zinc-400 hover:text-zinc-700 hover:bg-zinc-100 dark:hover:text-zinc-200 dark:hover:bg-zinc-800 disabled:opacity-30 disabled:pointer-events-none focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
          >
            <RotateCcw size={14} aria-hidden="true" />
          </button>
        </div>
      </div>
    </div>
  );
}
