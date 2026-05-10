import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { RotateCcw, X } from "lucide-react";
import {
  comboFromEvent,
  comboParts,
  DEFAULT_BINDINGS,
  loadBindings,
  saveBindings,
  SHORTCUT_ACTIONS,
  type ShortcutAction,
  type ShortcutBindings,
} from "../../../lib/shortcuts";

/**
 * Editor card for the keyboard-shortcut bindings. Click a row to enter
 * capture mode, press the desired combo; Backspace clears the binding,
 * Escape cancels. Conflicts (combo already assigned elsewhere) are
 * resolved by un-binding the previous owner so two actions can never
 * share the same combo.
 */
export function ShortcutsCard() {
  const { t } = useTranslation();
  const [bindings, setBindings] = useState<ShortcutBindings>(DEFAULT_BINDINGS);
  const [capturingAction, setCapturingAction] = useState<ShortcutAction | null>(
    null,
  );

  useEffect(() => {
    loadBindings()
      .then(setBindings)
      .catch((err) => console.error("[ShortcutsCard] load failed", err));
  }, []);

  const commit = useCallback((next: ShortcutBindings) => {
    setBindings(next);
    saveBindings(next).catch((err) =>
      console.error("[ShortcutsCard] save failed", err),
    );
  }, []);

  useEffect(() => {
    if (!capturingAction) return;
    const onKey = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        setCapturingAction(null);
        return;
      }
      if (event.key === "Backspace") {
        // Clear binding for this action.
        commit({ ...bindings, [capturingAction]: "" });
        setCapturingAction(null);
        return;
      }
      const combo = comboFromEvent(event);
      if (!combo) return;
      // Steal the combo from whoever else owns it.
      const next: ShortcutBindings = { ...bindings };
      for (const a of SHORTCUT_ACTIONS) {
        if (a !== capturingAction && next[a] === combo) {
          next[a] = "";
        }
      }
      next[capturingAction] = combo;
      commit(next);
      setCapturingAction(null);
    };
    // Capture phase so global shortcuts (also on window keydown) don't
    // get triggered while we're rebinding.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturingAction, bindings, commit]);

  const handleResetAll = useCallback(() => {
    commit({ ...DEFAULT_BINDINGS });
  }, [commit]);

  const handleUnbind = useCallback(
    (action: ShortcutAction) => {
      commit({ ...bindings, [action]: "" });
    },
    [bindings, commit],
  );

  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800/50 p-5">
      <div className="flex items-start justify-between mb-3">
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {t("settings.shortcuts.subtitle")}
        </p>
        <button
          type="button"
          onClick={handleResetAll}
          className="flex items-center space-x-1 text-xs font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 transition-colors"
        >
          <RotateCcw size={12} aria-hidden="true" />
          <span>{t("settings.shortcuts.reset")}</span>
        </button>
      </div>
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
        {SHORTCUT_ACTIONS.map((action) => {
          const combo = bindings[action];
          const isCapturing = capturingAction === action;
          return (
            <li key={action} className="py-2.5 flex items-center justify-between gap-3">
              <span className="text-sm text-zinc-700 dark:text-zinc-300">
                {t(`settings.shortcuts.actions.${action}`)}
              </span>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setCapturingAction(action)}
                  className={`min-w-32 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors border ${
                    isCapturing
                      ? "border-emerald-500 bg-emerald-50 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-300"
                      : "border-zinc-200 bg-zinc-50 text-zinc-700 hover:border-zinc-300 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:border-zinc-600"
                  }`}
                >
                  {isCapturing ? (
                    <span className="italic">
                      {t("settings.shortcuts.captureHint")}
                    </span>
                  ) : combo ? (
                    <span className="font-mono flex items-center justify-center gap-1">
                      {comboParts(combo).map((part, i) => (
                        <span key={i}>
                          {i > 0 && (
                            <span className="text-zinc-400 mx-0.5">+</span>
                          )}
                          <kbd className="px-1.5 py-0.5 rounded border border-zinc-300 bg-white text-zinc-700 dark:border-zinc-600 dark:bg-zinc-900 dark:text-zinc-200">
                            {part}
                          </kbd>
                        </span>
                      ))}
                    </span>
                  ) : (
                    <span className="text-zinc-400">
                      {t("settings.shortcuts.unbound")}
                    </span>
                  )}
                </button>
                {combo && !isCapturing && (
                  <button
                    type="button"
                    onClick={() => handleUnbind(action)}
                    aria-label={t("settings.shortcuts.unbind")}
                    title={t("settings.shortcuts.unbind")}
                    className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
                  >
                    <X size={14} />
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
