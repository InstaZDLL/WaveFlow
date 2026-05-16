import React from "react";
import ReactDOM from "react-dom/client";
import {
  getCurrentWindow,
  Window as TauriWindow,
} from "@tauri-apps/api/window";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import "./app.css";
import { i18nReady } from "./i18n";

// The mini-player runs in a second WebviewWindow that loads the same
// bundle with `?mini=1` in the URL. We branch here so it boots into
// a stripped-down provider tree (no LibraryContext / sidebar / etc).
const isMini = new URLSearchParams(window.location.search).get("mini") === "1";

// The main window is created with `visible: false` in tauri.conf.json so
// the user never sees a white WebView while Rust setup + React mount run.
// A `splashscreen` window is created in its place (small, transparent,
// always-on-top) to give visual feedback during the cold-start delay
// — especially on the very first launch after install, when Windows
// SmartScreen / Defender scans every freshly-extracted DLL.
//
// We reveal the main window after the first frame is painted, then
// close the splash. Order matters: show main BEFORE closing splash so
// there's never a moment where the desktop is visible between the two.
// The mini-player is its own window opened explicitly with visible:
// true, so skip the dance there.
function revealMainWindow() {
  if (isMini) return;
  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      void (async () => {
        try {
          await getCurrentWindow().show();
        } catch (err) {
          console.error("[main] window.show failed", err);
        }
        try {
          const splash = await TauriWindow.getByLabel("splashscreen");
          if (splash) await splash.close();
        } catch (err) {
          console.error("[main] splash close failed", err);
        }
      })();
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
    revealMainWindow();
  });
