import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Minus, Square, Copy, X } from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The title bar WaveFlow draws when the user asked it to (issue #696).
 *
 * Only mounted where the window has no frame of its own — Linux, where
 * `set_decorations(false)` took the GTK3 one away. macOS never gets this:
 * there the frame stays and only becomes transparent, because drawing our
 * own traffic lights is the one thing a macOS user would call *not*
 * native. See `commands/preferences.rs` for the platform split.
 *
 * The drag region is the whole strip minus the buttons, with an explicit
 * `startDragging()` on mousedown as well — the same belt-and-braces the
 * mini-player needs, because `data-tauri-drag-region` only fires when the
 * mousedown target itself carries the attribute, and pointer-events alone
 * has lost the race against the OS hit-test before.
 */
export function AppTitleBar() {
  const { t } = useTranslation();
  const [maximized, setMaximized] = useState(false);

  // The middle button changes meaning with the window's state, so it has
  // to follow a maximise that did not come from here either — a double
  // click on the strip, a keyboard shortcut, the window manager's own
  // controls.
  useEffect(() => {
    const appWindow = getCurrentWindow();
    let unlisten: (() => void) | null = null;
    let cancelled = false;

    appWindow
      .isMaximized()
      .then((value) => {
        if (!cancelled) setMaximized(value);
      })
      .catch(() => undefined);

    appWindow
      .onResized(() => {
        appWindow
          .isMaximized()
          .then((value) => {
            if (!cancelled) setMaximized(value);
          })
          .catch(() => undefined);
      })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch((err) => console.error("[AppTitleBar] onResized failed", err));

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const run = (action: "minimize" | "toggleMaximize" | "close") => () => {
    const appWindow = getCurrentWindow();
    const done =
      action === "minimize"
        ? appWindow.minimize()
        : action === "toggleMaximize"
          ? appWindow.toggleMaximize()
          : appWindow.close();
    done.catch((err) => console.error(`[AppTitleBar] ${action} failed`, err));
  };

  return (
    <div className="flex h-8 shrink-0 items-stretch border-b border-zinc-200/70 bg-transparent text-zinc-500 select-none dark:border-zinc-800/70 dark:text-zinc-400">
      <div
        data-tauri-drag-region
        onMouseDown={(e) => {
          if (e.button !== 0) return;
          // A double click is the window manager's maximise gesture, and
          // starting a drag would swallow it.
          if (e.detail > 1) return;
          getCurrentWindow()
            .startDragging()
            .catch((err) =>
              console.error("[AppTitleBar] startDragging failed", err),
            );
        }}
        onDoubleClick={run("toggleMaximize")}
        className="flex flex-1 items-center px-3 text-xs font-medium tracking-wide"
      >
        <span className="pointer-events-none">WaveFlow</span>
      </div>
      <div className="flex items-stretch">
        <TitleBarButton onClick={run("minimize")} label={t("window.minimize")}>
          <Minus size={14} />
        </TitleBarButton>
        <TitleBarButton
          onClick={run("toggleMaximize")}
          label={maximized ? t("window.restore") : t("window.maximize")}
        >
          {maximized ? <Copy size={12} /> : <Square size={12} />}
        </TitleBarButton>
        <TitleBarButton onClick={run("close")} label={t("common.close")} danger>
          <X size={14} />
        </TitleBarButton>
      </div>
    </div>
  );
}

function TitleBarButton({
  onClick,
  label,
  danger = false,
  children,
}: {
  onClick: () => void;
  label: string;
  /** Close gets the red hover every desktop gives it. */
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      className={`flex w-11 items-center justify-center transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-emerald-500 ${
        danger
          ? "hover:bg-red-500 hover:text-white"
          : "hover:bg-zinc-200/70 hover:text-zinc-900 dark:hover:bg-zinc-800/70 dark:hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}
