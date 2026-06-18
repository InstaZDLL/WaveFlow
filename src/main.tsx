import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import { ReadySignal } from "./components/common/ReadySignal";
import "./app.css";
// Self-hosted fonts for the Editorial skin (Playfair Display + Lora).
// Imported here so the @font-face declarations land at the top of the
// bundled CSS — they need to precede any rules that consume them, and
// importing from inside `editorial.css` would leave them stranded
// mid-bundle (illegal per spec, and PostCSS warns). Each fontsource
// CSS embeds latin + latin-ext + cyrillic + vietnamese subsets with
// `unicode-range`, so the browser only downloads the woff2 actually
// needed for the current locale. Files are bundled into the app —
// zero network at runtime, works offline.
import "@fontsource/playfair-display/400-italic.css";
import "@fontsource/playfair-display/900.css";
import "@fontsource/lora/400.css";
import "@fontsource/lora/400-italic.css";
import "@fontsource/lora/500.css";
import "@fontsource/lora/700.css";
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
