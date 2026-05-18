import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import { ReadySignal } from "./components/common/ReadySignal";
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
// in src-tauri/src/lib.rs). The actual event emission lives in
// `ReadySignal` so this entry-point file can stay HMR-friendly.

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
