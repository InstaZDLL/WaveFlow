import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { useTranslation } from "react-i18next";
import { SlidersHorizontal, Check, ChevronDown } from "lucide-react";
import {
  playerGetEq,
  playerSetEqEnabled,
  playerSetEqPreset,
  type EqPresetEntry,
} from "../../lib/tauri/eq";

/**
 * Compact preset picker for the player bar (and the "⋯" overflow
 * menu via [`EqPresetPanel`] below). Wraps the same backend
 * commands as [`EqualizerCard`](../views/settings/EqualizerCard.tsx)
 * but skips the draggable curve — users who want the full editor
 * still open Settings → Lecture.
 *
 * Layout: icon button → popover with a bypass toggle row at top and
 * a scrollable list of presets (20 built-ins) underneath. The same
 * `EqPresetPanel` body is rendered inline by the overflow menu when
 * the user hasn't pinned the EQ button to the bar.
 */
export function EqPresetButton() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const popoverRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  // Position the popover above the trigger — the player bar lives at
  // the bottom of the viewport so a downward-opening popover would
  // clip below the screen.
  const popoverStyle: CSSProperties = {
    bottom: "calc(100% + 8px)",
    right: 0,
  };

  // Outside-click + Escape to dismiss. Trigger excluded so the user
  // can click it again to close without immediately re-opening.
  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      const target = e.target as Node | null;
      if (!target) return;
      if (popoverRef.current?.contains(target)) return;
      if (triggerRef.current?.contains(target)) return;
      setOpen(false);
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", handleClick);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [open]);

  return (
    <div className="relative">
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-label={t("playerBar.eqPreset")}
        title={t("playerBar.eqPreset")}
        aria-expanded={open}
        className={`p-1.5 rounded-lg transition-colors ${
          open
            ? "text-emerald-500 bg-emerald-500/10"
            : "text-zinc-400 hover:text-zinc-800 dark:hover:text-white"
        }`}
      >
        <SlidersHorizontal size={20} />
      </button>
      {open && (
        <div
          ref={popoverRef}
          role="dialog"
          aria-label={t("playerBar.eqPreset")}
          style={popoverStyle}
          className="absolute z-50 w-56 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-xl overflow-hidden"
        >
          <EqPresetPanel onPick={() => setOpen(false)} />
        </div>
      )}
    </div>
  );
}

interface EqPresetPanelProps {
  /** Called after the user picks a preset. The popover variant uses
   *  it to close itself; the inline-in-menu variant in
   *  `MoreActionsMenu` uses it to dismiss the parent menu. */
  onPick?: () => void;
  /** When `true`, render a collapsed "current preset" button instead
   *  of the full scrollable list. The list expands inline on click.
   *  Used by the overflow menu to keep the popover compact on
   *  smaller (1080p) displays. */
  collapsible?: boolean;
}

/**
 * Bare panel content — bypass toggle row + preset list. Shared
 * between the popover variant (`EqPresetButton` above) and the
 * inline-in-menu variant (rendered by `MoreActionsMenu` when the EQ
 * pin is OFF). Hydrates EQ state lazily on mount so closing then
 * re-opening always reflects the latest backend snapshot (e.g. user
 * changed preset from the full EqualizerCard in Settings).
 *
 * Pass `collapsible` to hide the 20-row preset list behind a
 * dropdown trigger that shows the current preset name. The list
 * stays inline (no nested popover) so the parent menu's max-height +
 * scroll keeps governing the layout.
 */
export function EqPresetPanel({ onPick, collapsible = false }: EqPresetPanelProps) {
  const { t } = useTranslation();
  const [enabled, setEnabled] = useState(false);
  const [presets, setPresets] = useState<EqPresetEntry[]>([]);
  const [bands, setBands] = useState<number[]>([0, 0, 0, 0, 0, 0]);
  // Only relevant when `collapsible` is true. The list stays hidden
  // until the user clicks the current-preset row to expand it.
  const [listExpanded, setListExpanded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    playerGetEq()
      .then((snap) => {
        if (cancelled) return;
        setEnabled(snap.enabled);
        setPresets(snap.presets);
        setBands(snap.bands_db);
      })
      .catch((err) => console.error("[EqPresetPanel] hydrate failed", err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Active-preset detection mirrors EqualizerCard (exact-gain match
  // within 0.01 dB). A user who nudged the full editor falls back to
  // `custom` and no row is highlighted.
  const activeKey = useMemo(() => {
    const match = presets.find((p) =>
      p.gains.every((g, i) => Math.abs((bands[i] ?? 0) - g) < 0.01),
    );
    return match?.key ?? "custom";
  }, [presets, bands]);

  const handleToggle = useCallback(() => {
    const next = !enabled;
    setEnabled(next);
    playerSetEqEnabled(next).catch((err) => {
      console.error("[EqPresetPanel] toggle failed", err);
      setEnabled(!next);
    });
  }, [enabled]);

  const handlePick = useCallback(
    (key: string) => {
      const preset = presets.find((p) => p.key === key);
      if (!preset) return;
      setBands(preset.gains);
      playerSetEqPreset(key).catch((err) =>
        console.error("[EqPresetPanel] preset failed", err),
      );
      // In compact mode the user picked from the expanded list — fold
      // it back up so the menu returns to its tight default layout.
      if (collapsible) setListExpanded(false);
      onPick?.();
    },
    [presets, onPick, collapsible],
  );

  const activePresetLabel =
    activeKey === "custom"
      ? t("settings.equalizer.preset.custom", { defaultValue: "Custom" })
      : t(`settings.equalizer.preset.${activeKey}`, { defaultValue: activeKey });

  const bypassRow = (
    <div className="flex items-center justify-between px-3 py-2 border-b border-zinc-100 dark:border-zinc-800">
      <span className="text-xs font-medium text-zinc-700 dark:text-zinc-200">
        {t("playerBar.eq.bypass")}
      </span>
      <button
        type="button"
        role="switch"
        aria-checked={enabled}
        onClick={handleToggle}
        className={`relative h-5 w-9 rounded-full transition-colors ${
          enabled ? "bg-emerald-500" : "bg-zinc-300 dark:bg-zinc-700"
        }`}
      >
        <span
          className={`absolute top-0.5 h-4 w-4 rounded-full bg-white shadow transition-transform ${
            enabled ? "translate-x-4" : "translate-x-0.5"
          }`}
        />
      </button>
    </div>
  );

  const presetList = (
    <ul className="max-h-64 overflow-y-auto py-1" role="listbox">
      {presets.map((preset) => {
        const isActive = preset.key === activeKey;
        return (
          <li key={preset.key}>
            <button
              type="button"
              role="option"
              aria-selected={isActive}
              onClick={() => handlePick(preset.key)}
              className={`w-full flex items-center justify-between px-3 py-1.5 text-sm text-left transition-colors ${
                isActive
                  ? "text-emerald-600 dark:text-emerald-400 bg-emerald-500/5"
                  : "text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              <span className="truncate">
                {t(`settings.equalizer.preset.${preset.key}`, {
                  defaultValue: preset.key,
                })}
              </span>
              {isActive && <Check size={14} aria-hidden="true" />}
            </button>
          </li>
        );
      })}
    </ul>
  );

  if (!collapsible) {
    return (
      <>
        {bypassRow}
        {presetList}
      </>
    );
  }

  return (
    <>
      {bypassRow}
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={listExpanded}
        onClick={() => setListExpanded((v) => !v)}
        className="w-full flex items-center justify-between gap-2 px-3 py-2 text-sm text-left text-zinc-700 dark:text-zinc-200 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition-colors"
      >
        <span className="truncate">{activePresetLabel}</span>
        <ChevronDown
          size={14}
          aria-hidden="true"
          className={`shrink-0 transition-transform ${listExpanded ? "rotate-180" : ""}`}
        />
      </button>
      {listExpanded && (
        <div className="border-t border-zinc-100 dark:border-zinc-800">
          {presetList}
        </div>
      )}
    </>
  );
}
