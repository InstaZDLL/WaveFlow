/**
 * Which of WaveFlow's webviews this document is running in.
 *
 * Every window loads the same bundle; the secondary ones are told apart
 * by a query flag. `main.tsx` picks the provider tree from it, and
 * anything that must exist once per app rather than once per webview
 * (the Spotify Web Playback SDK, which registers a device) checks
 * {@link IS_SECONDARY_WINDOW} so it only runs in the main window.
 *
 * - `mini`: the always-on-top mini-player (`?mini=1`).
 * - `lyrics`: the floating desktop lyrics overlay (`?lyrics=1`, #582),
 *   created by the backend.
 */
export type WindowRole = "main" | "mini" | "lyrics";

function readRole(): WindowRole {
  if (typeof window === "undefined") return "main";
  const params = new URLSearchParams(window.location.search);
  if (params.get("mini") === "1") return "mini";
  if (params.get("lyrics") === "1") return "lyrics";
  return "main";
}

export const WINDOW_ROLE: WindowRole = readRole();

export const IS_SECONDARY_WINDOW = WINDOW_ROLE !== "main";
