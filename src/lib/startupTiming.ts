/**
 * Where the time goes between the webview navigating and React's first
 * commit (#626).
 *
 * The backend already knows when it launched and when the frontend
 * reported ready. Three launches measured with only those two numbers
 * showed the frontend arriving 20 to 27 seconds after navigation, with
 * `setup` long since finished — so the delay is inside the webview, and
 * the question became *which part*. These marks split it:
 *
 * - `bundleMs` — the entry module is executing, so the document was
 *   fetched, parsed, and its imports resolved.
 * - `i18nMs` — i18next has loaded its locale. `main.tsx` renders inside
 *   `i18nReady.finally(...)`, so nothing can commit before this.
 * - the ready signal itself — React committed.
 *
 * All three are `performance.now()`, i.e. milliseconds since navigation,
 * and they ride along on the `app_ready` call so one log line carries the
 * whole timeline. Cheap enough to leave in: three number reads and one
 * extra field on a call that happens once per launch.
 */

let bundleMs: number | null = null;
let i18nMs: number | null = null;

/** Called from the entry module's body, as early as it can be. */
export function markBundleReady(): void {
  bundleMs ??= Math.round(performance.now());
}

/** Called once i18next has resolved, just before the first render. */
export function markI18nReady(): void {
  i18nMs ??= Math.round(performance.now());
}

export interface StartupTimings {
  bundleMs: number | null;
  i18nMs: number | null;
}

export function readStartupTimings(): StartupTimings {
  return { bundleMs, i18nMs };
}
