import React from "react";
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
// The mini-player is opened with visible: true so it's already on
// screen by the time React mounts — skip the signal there.
function signalReady() {
  if (isMini) return;
  // Two rAFs to let React commit + the compositor paint at least one
  // useful frame before we reveal the main window. The backend has a
  // 15 s safety-net timer so even if this never fires (e.g. a frontend
  // crash before render) the user is not stuck on the splash.
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void emit("app://ready").catch((err) => {
        console.error("[main] emit(app://ready) failed", err);
      });
    });
  });
}

i18nReady
  .catch((err) => {
    console.error("[i18n] initialization failed", err);
  })
  .finally(() => {
    ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
      <React.StrictMode>
        {isMini ? <MiniPlayerApp /> : <App />}
      </React.StrictMode>,
    );
    signalReady();
  });
