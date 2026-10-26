import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Info, Loader2, X } from "lucide-react";
import {
  clearGlobalShortcuts,
  GLOBAL_SHORTCUT_ACTIONS,
  globalComboFromEvent,
  globalComboParts,
  globalShortcutsStatus,
  setGlobalShortcut,
  suspendGlobalShortcuts,
  type GlobalShortcutAction,
  type GlobalShortcutState,
  type GlobalShortcutsSnapshot,
} from "../../../lib/tauri/globalShortcuts";

/** A combination tried and not kept, or a key that cannot be used. */
interface Refusal {
  action: GlobalShortcutAction;
  combo: string | null;
  state: GlobalShortcutState;
}

/**
 * Editor for the system-wide shortcuts. Same gesture as the in-app card —
 * click a row, press the combination; Backspace unbinds, Escape cancels —
 * but nothing is bound by default, and each binding says where it stands:
 * a combination another application holds is refused and named as such
 * rather than stored dead. On a Wayland desktop the desktop confirms each
 * change in its own dialog; without its portal the card says the feature
 * is unavailable instead of accepting combinations that would never fire.
 */
export function GlobalShortcutsCard() {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<GlobalShortcutsSnapshot | null>(
    null,
  );
  const [capturing, setCapturing] = useState<GlobalShortcutAction | null>(null);
  const [pending, setPending] = useState<GlobalShortcutAction | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const capturingRef = useRef<GlobalShortcutAction | null>(null);

  useEffect(() => {
    globalShortcutsStatus()
      .then(setSnapshot)
      .catch((err) => console.error("[GlobalShortcutsCard] load failed", err));
  }, []);

  const apply = useCallback(
    async (action: GlobalShortcutAction, combo: string | null) => {
      setPending(action);
      try {
        const next = await setGlobalShortcut(action, combo);
        setSnapshot(next);
        setRefusal(next.rejected);
      } catch (err) {
        console.error("[GlobalShortcutsCard] save failed", err);
      } finally {
        setPending(null);
      }
    },
    [],
  );

  const stopCapture = useCallback((resume: boolean) => {
    capturingRef.current = null;
    setCapturing(null);
    if (resume) {
      suspendGlobalShortcuts(false)
        .then(setSnapshot)
        .catch((err) =>
          console.error("[GlobalShortcutsCard] resume failed", err),
        );
    }
  }, []);

  const startCapture = useCallback((action: GlobalShortcutAction) => {
    setRefusal(null);
    capturingRef.current = action;
    setCapturing(action);
    suspendGlobalShortcuts(true)
      .then((next) => {
        // A capture that ended in the meantime has resumed already.
        if (capturingRef.current) setSnapshot(next);
      })
      .catch((err) =>
        console.error("[GlobalShortcutsCard] suspend failed", err),
      );
  }, []);

  useEffect(() => {
    if (!capturing) return;
    const onKey = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.repeat) return;
      const bare =
        !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey;
      if (event.key === "Escape" && bare) {
        stopCapture(true);
        return;
      }
      if (event.key === "Backspace" && bare) {
        // `set` ends the recording itself.
        stopCapture(false);
        void apply(capturing, null);
        return;
      }
      const combo = globalComboFromEvent(event);
      if (combo === "") return;
      if (combo === null) {
        setRefusal({
          action: capturing,
          combo: null,
          state: { kind: "invalid", reason: "unsupportedKey" },
        });
        stopCapture(true);
        return;
      }
      stopCapture(false);
      void apply(capturing, combo);
    };
    // Leaving the window mid-recording puts the shortcuts back.
    const onBlur = () => stopCapture(true);
    // Capture phase, so the in-app shortcuts do not fire meanwhile.
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", onBlur);
    };
  }, [capturing, apply, stopCapture]);

  // Leaving the page mid-recording puts the shortcuts back too.
  useEffect(
    () => () => {
      if (capturingRef.current) void suspendGlobalShortcuts(false);
    },
    [],
  );

  const handleClearAll = useCallback(() => {
    setRefusal(null);
    clearGlobalShortcuts()
      .then(setSnapshot)
      .catch((err) => console.error("[GlobalShortcutsCard] clear failed", err));
  }, []);

  const stateMessage = (state: GlobalShortcutState): string | null => {
    switch (state.kind) {
      case "active":
      case "suspended":
        return null;
      case "invalid":
        return t(`settings.globalShortcuts.states.invalid.${state.reason}`);
      default:
        return t(`settings.globalShortcuts.states.${state.kind}`);
    }
  };

  const backend = snapshot?.backend ?? "native";
  const unavailable = backend === "unavailable";
  const hasBindings = (snapshot?.bindings.length ?? 0) > 0;

  return (
    <div className="rounded-2xl border border-zinc-200 bg-white dark:border-zinc-700 dark:bg-zinc-800/50 p-5">
      <div className="flex items-start justify-between gap-4 mb-3">
        <p className="text-xs settings-description">
          {t("settings.globalShortcuts.subtitle")}
        </p>
        {hasBindings && !capturing && (
          <button
            type="button"
            onClick={handleClearAll}
            className="shrink-0 text-xs font-medium text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200 transition-colors"
          >
            {t("settings.globalShortcuts.clearAll")}
          </button>
        )}
      </div>
      {backend !== "native" && (
        <p
          role="note"
          className={`mb-3 flex items-start gap-2 rounded-lg px-3 py-2 text-xs ${
            unavailable
              ? "bg-amber-50 text-amber-800 dark:bg-amber-900/20 dark:text-amber-200"
              : "bg-zinc-50 text-zinc-600 dark:bg-zinc-800 dark:text-zinc-300"
          }`}
        >
          <Info size={14} className="mt-px shrink-0" aria-hidden="true" />
          <span>{t(`settings.globalShortcuts.backend.${backend}`)}</span>
        </p>
      )}
      {!unavailable && (
        <p className="mb-2 text-xs text-zinc-500 dark:text-zinc-400">
          {t("settings.globalShortcuts.captureHelp")}
        </p>
      )}
      <ul className="divide-y divide-zinc-100 dark:divide-zinc-700/50">
        {GLOBAL_SHORTCUT_ACTIONS.map((action) => {
          const binding = snapshot?.bindings.find((b) => b.action === action);
          const isCapturing = capturing === action;
          const isPending = pending === action;
          const rowRefusal = refusal?.action === action ? refusal : null;
          const message = rowRefusal
            ? rowRefusal.combo
              ? t("settings.globalShortcuts.rejected", {
                  combo: globalComboParts(rowRefusal.combo).join("+"),
                  reason: stateMessage(rowRefusal.state),
                })
              : stateMessage(rowRefusal.state)
            : binding
              ? stateMessage(binding.state)
              : null;
          const trigger =
            binding?.trigger && binding.state.kind === "active"
              ? binding.trigger
              : null;
          return (
            <li key={action} className="py-2.5">
              <div className="flex items-center justify-between gap-3">
                <span className="text-sm text-zinc-700 dark:text-zinc-300">
                  {t(`settings.shortcuts.actions.${action}`)}
                </span>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    disabled={unavailable || isPending}
                    onClick={() =>
                      isCapturing ? stopCapture(true) : startCapture(action)
                    }
                    aria-describedby={
                      message || trigger
                        ? `global-shortcut-status-${action}`
                        : undefined
                    }
                    className={`min-w-32 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors border disabled:opacity-50 disabled:cursor-not-allowed ${
                      isCapturing
                        ? "border-emerald-500 bg-emerald-50 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-300"
                        : "border-zinc-200 bg-zinc-50 text-zinc-700 hover:border-zinc-300 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:border-zinc-600"
                    }`}
                  >
                    {isPending ? (
                      <span className="flex items-center justify-center gap-1.5">
                        <Loader2
                          size={12}
                          className="animate-spin"
                          aria-hidden="true"
                        />
                        <span>
                          {backend === "portal"
                            ? t("settings.globalShortcuts.waitingForDesktop")
                            : t("settings.globalShortcuts.registering")}
                        </span>
                      </span>
                    ) : isCapturing ? (
                      <span className="italic">
                        {t("settings.shortcuts.captureHint")}
                      </span>
                    ) : binding ? (
                      <span className="font-mono flex items-center justify-center gap-1">
                        {globalComboParts(binding.combo).map((part, i) => (
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
                  {/* Hidden during any recording: unbinding stores the
                      set, which puts the suspended registrations back
                      while the recording row still waits for its keys. */}
                  {binding && !capturing && !isPending && (
                    <button
                      type="button"
                      onClick={() => void apply(action, null)}
                      aria-label={t("settings.shortcuts.unbind")}
                      title={t("settings.shortcuts.unbind")}
                      className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
                    >
                      <X size={14} />
                    </button>
                  )}
                </div>
              </div>
              {(message || trigger) && (
                <p
                  id={`global-shortcut-status-${action}`}
                  role={rowRefusal ? "alert" : undefined}
                  className={`mt-1 text-xs ${
                    message
                      ? "text-amber-700 dark:text-amber-300"
                      : "text-zinc-500 dark:text-zinc-400"
                  }`}
                >
                  {message ??
                    t("settings.globalShortcuts.assigned", { trigger })}
                </p>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
