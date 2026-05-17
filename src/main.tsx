import React, { useEffect } from "react";
import ReactDOM from "react-dom/client";
import { emit } from "@tauri-apps/api/event";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import "./app.css";
import { i18nReady } from "./i18n";

// The mini-player runs in a second WebviewWindow that loads the same
// bundle with `?mini=1` in the URL. We branch here so it boots into
// a stripped-down provider tree (no LibraryContext / sidebar / etc).
const isMini = new URLSearchParams(window.location.search).get("mini") === "1";

// The main window is created with `visible: false` in tauri.conf.json
// so the user never sees a white WebView while Rust setup + React mount
// run. A `splashscreen` window is shown in its place. The backend
// listens for `app://ready` and atomically reveals the main window +
// closes the splash from native code (see `reveal_main_close_splash`
// in src-tauri/src/lib.rs).
//
// Doing the handoff in native code rather than IPC avoids the race
// that bit issue #42 on Linux WebKitGTK 2.52: the previous frontend
// rAF-driven `window.show()` could fire before React had committed
// anything, leaving the user staring at an empty splash forever.
//
// We emit from a useEffect at the React root rather than from a 2-rAF
// dance because WebKitGTK 2.52 suspends `requestAnimationFrame`
// callbacks while a window is hidden — `visible: false` means rAF
// never fires until the backend reveals the window, deadlocking the
// handoff until the 15 s safety-net timer trips. useEffect runs after
// the first React commit, which is the actual guarantee we care about
// (DOM is populated before reveal); the compositor will paint the
// first frame as part of the reveal itself, so we don't need to
// observe a paint to avoid a flash.
function ReadySignal({ children }: { children: React.ReactNode }) {
  useEffect(() => {
    void emit("app://ready").catch((err) => {
      console.error("[main] emit(app://ready) failed", err);
    });
  }, []);
  return <>{children}</>;
}

i18nReady
  .catch((err) => {
    console.error("[i18n] initialization failed", err);
  })
  .finally(() => {
    const root = ReactDOM.createRoot(
      document.getElementById("root") as HTMLElement,
    );
    root.render(
      <React.StrictMode>
        {isMini ? (
          <MiniPlayerApp />
        ) : (
          <ReadySignal>
            <App />
          </ReadySignal>
        )}
      </React.StrictMode>,
    );
  });
