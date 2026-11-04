/**
 * The webview's own right-click menu stays out of the app.
 *
 * Unless a page says otherwise, each engine opens its browser menu: Back,
 * Refresh, Save as and Print under WebView2, Back, Forward, Stop and Reload
 * under WebKitGTK, Reload under WKWebView, and Inspect in a debug build. None
 * of it means anything in a player, and a reload starts the whole interface
 * over.
 *
 * Only the default is prevented, and during capture: the app's own menus
 * still get the event and open as they did, and a handler that stops its
 * propagation cannot let the browser menu through.
 *
 * A text field keeps its menu, for copy, paste and the spell checker. Under
 * `tauri dev`, Shift + right click brings the browser menu back everywhere,
 * over the app's own menus too since they never get the event, which is the
 * way to Inspect there.
 *
 * Imported once, for its listener, from `main.tsx`, which every window loads.
 */

/** The `<input>` types that take no text, so have nothing to copy or paste. */
const NON_TEXT_INPUTS = new Set([
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
]);

function isTextField(target: EventTarget | null): boolean {
  if (target instanceof HTMLInputElement) {
    return !NON_TEXT_INPUTS.has(target.type);
  }
  return (
    target instanceof HTMLTextAreaElement ||
    (target instanceof HTMLElement && target.isContentEditable)
  );
}

if (typeof document !== "undefined") {
  document.addEventListener(
    "contextmenu",
    (event) => {
      if (isTextField(event.target)) return;
      if (import.meta.env.DEV && event.shiftKey) {
        event.stopPropagation();
        return;
      }
      event.preventDefault();
    },
    true,
  );
}
