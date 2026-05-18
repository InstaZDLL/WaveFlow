import { useEffect, type ReactNode } from "react";
import { emit } from "@tauri-apps/api/event";

/**
 * Emits `app://ready` once after the first React commit so the Rust
 * backend can reveal the main window and close the splash from native
 * code (see `reveal_main_close_splash` in src-tauri/src/lib.rs).
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
 * Lives in its own file rather than in `main.tsx` so it satisfies
 * the React Fast Refresh constraint ("a file must only export
 * components"). Bundle entry points like `main.tsx` have non-component
 * side effects (root.render, i18n init) that prevent HMR from
 * extracting the component cleanly.
 */
export function ReadySignal({ children }: { children: ReactNode }) {
  useEffect(() => {
    void emit("app://ready").catch((err) => {
      console.error("[ReadySignal] emit(app://ready) failed", err);
    });
  }, []);
  return <>{children}</>;
}
