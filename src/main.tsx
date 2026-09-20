import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import { DesktopLyricsApp } from "./DesktopLyricsApp";
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
// Pulse skin — Space Grotesk for display + body, Space Mono for
// the tech-track utility chrome (eyebrows, nav pills, time codes,
// the `///` section markers). Loaded eagerly so the skin doesn't
// FOUT when the user flips into it from Studio.
import "@fontsource/space-grotesk/400.css";
import "@fontsource/space-grotesk/700.css";
import "@fontsource/space-mono/400.css";
import "@fontsource/space-mono/700.css";
// Liquid skin — DM Sans Variable (opsz axis). The variable
// font carries both the weight axis (100-1000) and the optical-
// sizing axis (9-40), so the same family scales from caption-
// precise rendering at small sizes to display-generous
// rendering at large sizes — the property the comment on
// liquid.css promises. Single woff2 (~100-150 KB latin) instead
// of four static weight files, plus zero glyph mismatch when
// any new size shows up in the UI.
import "@fontsource-variable/dm-sans/opsz.css";
import { i18nReady } from "./i18n";
import { markBundleReady, markI18nReady } from "./lib/startupTiming";
import { WINDOW_ROLE } from "./lib/windowRole";

// First statement that runs once the entry module is executing: the
// document and every static import above are in (#626).
markBundleReady();

// The mini-player (`?mini=1`) and the desktop lyrics overlay (`?lyrics=1`,
// #582) run in their own WebviewWindows that load the same bundle. We
// branch here so each boots into a stripped-down provider tree (no
// LibraryContext / sidebar / etc).
// The overlay's document must be transparent from its very first paint;
// see `html.desktop-lyrics-window` in app.css.
if (WINDOW_ROLE === "lyrics") {
  document.documentElement.classList.add("desktop-lyrics-window");
}
// The mini-player is a fixed-size widget: nothing in it is ever meant to
// scroll the page, and at 280 px wide a scrollbar is a visible chunk of it.
// See `html.mini-player-window` in app.css for what was producing one.
if (WINDOW_ROLE === "mini") {
  document.documentElement.classList.add("mini-player-window");
}

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
    // Nothing can commit before this resolves, so it is the second half of
    // the split (#626).
    markI18nReady();
    const root = ReactDOM.createRoot(
      document.getElementById("root") as HTMLElement,
    );
    root.render(
      <React.StrictMode>
        {WINDOW_ROLE === "mini" ? (
          <MiniPlayerApp />
        ) : WINDOW_ROLE === "lyrics" ? (
          <DesktopLyricsApp />
        ) : (
          <ReadySignal>
            <App />
          </ReadySignal>
        )}
      </React.StrictMode>,
    );
  });
