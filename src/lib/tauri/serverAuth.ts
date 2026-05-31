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
 * Open the configured server URL in the user's default browser. The
 * user is expected to sign in there (Better Auth handles the actual
 * flow) and copy the JWT back into the Settings card. A future
 * `1.f.desktop.1b` PR replaces this with a local-loopback OAuth flow
 * mirroring the existing Spotify pattern.
 */
export function serverOpenLoginBrowser(): Promise<void> {
  return invoke<void>("server_open_login_browser");
}
