import { invoke } from "@tauri-apps/api/core";

/**
 * Snapshot of the desktop's binding to `waveflow-server` for the
 * Settings card. `url` is the base URL the desktop will hit for
 * `/api/v1/*` calls (app-wide). `signedIn` is `true` when the active
 * profile has a JWT row under the `waveflow_server` provider — it
 * does NOT validate the token against the server, which would cost an
 * HTTP round-trip the UI rarely needs.
 */
export interface ServerStatus {
  url: string | null;
  /**
   * waveflow-web URL the OAuth-loopback handshake (Phase
   * 1.f.desktop.1b) opens in the system browser. May differ from
   * `url` for deployments that proxy the API and the web on separate
   * domains.
   */
  web_url: string | null;
  signed_in: boolean;
}

/** Snapshot the server binding state. */
export function serverGetStatus(): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_get_status");
}

/**
 * Persist the waveflow-server base URL. Empty string clears the row
 * (back to local-only mode). Validates parseability + http(s) scheme
 * server-side, so a typo surfaces as an error rather than silently
 * breaking future sync calls.
 */
export function serverSetUrl(url: string): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_set_url", { url });
}

/** Persist the waveflow-web URL used by the OAuth-loopback flow. */
export function serverSetWebUrl(url: string): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_set_web_url", { url });
}

/**
 * Persist the Bearer JWT the user pasted in from the browser
 * sign-in. Rejects empty / non-JWT-shaped input server-side.
 */
export function serverSetToken(token: string): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_set_token", { token });
}

/** Drop the per-profile JWT. URL stays so the user can re-paste. */
export function serverSignOut(): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_sign_out");
}

/**
 * Open the configured server URL in the user's default browser —
 * kept as a fallback for users who prefer to copy the JWT manually.
 * The OAuth-loopback flow below is the default since 1.f.desktop.1b.
 */
export function serverOpenLoginBrowser(): Promise<void> {
  return invoke<void>("server_open_login_browser");
}

/**
 * Run the local-loopback OAuth-style handshake against `waveflow-web`.
 * Opens the system browser to `<web-url>/desktop-login?cb=…&state=…`,
 * binds a one-shot listener on 127.0.0.1:49388 for up to three
 * minutes, and persists the JWT the web side hands back. Returns the
 * fresh `ServerStatus` once the round-trip completes.
 *
 * Throws on user cancellation, timeout, or `state` mismatch (treated
 * as a possible CSRF — the UI surfaces the error string verbatim).
 */
export function serverBeginLoopbackLogin(): Promise<ServerStatus> {
  return invoke<ServerStatus>("server_begin_loopback_login");
}

/**
 * Sync mode for the active profile. `local` skips the
 * sync_pending_op queue entirely even when a JWT is configured
 * (useful for privacy-conscious profiles); `hybrid` is the default
 * once signed in — reads stay local, writes hit local + the queue,
 * and the drain task posts them upstream.
 */
export type SyncMode = "local" | "hybrid";

export function syncGetMode(): Promise<SyncMode> {
  return invoke<SyncMode>("sync_get_mode");
}

export function syncSetMode(mode: SyncMode): Promise<SyncMode> {
  return invoke<SyncMode>("sync_set_mode", { req: { mode } });
}
