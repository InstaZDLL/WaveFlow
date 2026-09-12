import { useEffect, type ReactNode } from "react";
import { emit } from "@tauri-apps/api/event";
import { markFrontendReady } from "../../lib/tauri/lifecycle";

/**
 * Tells the Rust backend, once after the first React commit, that the
 * frontend has rendered — so it can reveal the main window and close the
 * splash from native code (see `reveal_main_close_splash` and
 * `commands::ready` in src-tauri).
 *
 * We rely on a `useEffect` rather than a `requestAnimationFrame` dance
 * because WebKitGTK 2.52 suspends rAF callbacks while a window is
 * `visible: false` — rAF would never fire until the backend reveals
 * the window, deadlocking the handoff until the 15 s safety-net timer
 * trips. `useEffect` runs after React commits, which is the actual
 * guarantee we care about (DOM is populated before reveal); the
 * compositor will paint the first frame as part of the reveal itself,
 * so we don't need to observe a paint to avoid a flash.
 *
 * The signal goes through the `app_ready` **command**, retried a few times
 * if the call rejects. The `app://ready` event it used to send follows as
 * a second transport: the backend registers its listener part-way through
 * `setup`, so an event handled before that point is dropped and the user
 * waits out the full 15 s safety net (#626). The rendezvous is one-shot
 * and ignores duplicates, so sending both costs nothing.
 *
 * Lives in its own file rather than in `main.tsx` so it satisfies
 * the React Fast Refresh constraint ("a file must only export
 * components"). Bundle entry points like `main.tsx` have non-component
 * side effects (root.render, i18n init) that prevent HMR from
 * extracting the component cleanly.
 */
export function ReadySignal({ children }: { children: ReactNode }) {
  useEffect(() => {
    let cancelled = false;

    const announce = async () => {
      // Measured here because this is the half the backend cannot see:
      // navigation to first React commit, which includes the i18next gate
      // `main.tsx` renders behind.
      const sinceNavigationMs = Math.round(performance.now());
      for (let attempt = 0; attempt < 5 && !cancelled; attempt += 1) {
        try {
          await markFrontendReady(sinceNavigationMs);
          return;
        } catch (err) {
          console.error(
            `[ReadySignal] app_ready failed (attempt ${attempt + 1})`,
            err,
          );
          await new Promise((resolve) => setTimeout(resolve, 200));
        }
      }
    };

    void announce();
    void emit("app://ready").catch((err) => {
      console.error("[ReadySignal] emit(app://ready) failed", err);
    });

    return () => {
      cancelled = true;
    };
  }, []);
  return <>{children}</>;
}
