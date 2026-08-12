import { invoke } from "@tauri-apps/api/core";

/**
 * Binding between the active profile and a remote music server
 * (RFC-005). Replaces the `serverAuth.ts` surface, which talks to a
 * server generation that no longer exists.
 *
 * These commands only exist in a build with the `sync_v2` Cargo feature
 * on, which is not the default while the slices land. Calling them
 * against a stock build rejects with "command not found" — mount the UI
 * behind the same switch rather than catching that at each call site.
 */

/** Which protocol the bound server speaks. */
export type RemoteFlavour = "waveflow" | "subsonic";

export interface RemoteStatus {
  /** `null` means this profile is local-only — the normal state. */
  server_url: string | null;
  flavour: RemoteFlavour | null;
  /** The signed-in account name, when known. */
  username: string | null;
  /**
   * Usable credentials exist. Does NOT prove the server still accepts
   * them; that costs a round-trip the UI rarely needs.
   */
  signed_in: boolean;
  /**
   * A first snapshot has been applied at least once. Tells an empty
   * account apart from a bootstrap that never ran — without it the two
   * render identically.
   */
  bootstrapped: boolean;
}

export interface RemoteProbeResult {
  flavour: RemoteFlavour;
  /** Whatever the server calls itself, verbatim, for display. */
  server_type: string | null;
  server_version: string | null;
  /**
   * Whether the journal, per-device acknowledgement and mutation
   * idempotency exist here. Only a native server offers them.
   */
  supports_sync: boolean;
}

/** Snapshot the binding for the Settings card. */
export function remoteGetStatus(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_get_status");
}

/**
 * Identify a server without signing in.
 *
 * Worth doing before showing any form: a native server signs in through
 * the browser, a third-party one with a username and password, and
 * asking for the wrong one first is a dead end for the user. The probe
 * needs no credentials — the server identifies itself even on a failed
 * ping.
 *
 * Rejects when nothing at that URL answers like a music server.
 */
export function remoteDetectServer(url: string): Promise<RemoteProbeResult> {
  return invoke<RemoteProbeResult>("remote_detect_server", { url });
}

/**
 * Run the browser handshake (Authorization Code + PKCE) and bind the
 * active profile.
 *
 * Resolves only once the user has consented in their browser, so this
 * can sit pending for up to three minutes. Show it as pending, not as a
 * frozen button.
 *
 * Rejects on refusal, timeout, or a reply that does not match the
 * request. Each rejection carries a message meant to be shown as-is.
 */
export function remoteBeginLogin(url: string): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_begin_login", { url });
}

/**
 * Drop the credentials and nothing else. The cached remote library
 * stays readable and signing back into the same account resumes from
 * its cursor rather than re-downloading everything.
 */
export function remoteSignOut(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_sign_out");
}

/**
 * Drop the credentials, the binding, the cached library **and any
 * change made offline that never reached the server**. Destructive —
 * present it as such, and prefer {@link remoteSignOut} whenever the
 * intent is merely to stop being signed in.
 */
export function remoteForgetServer(): Promise<RemoteStatus> {
  return invoke<RemoteStatus>("remote_forget_server");
}

export interface RemoteSyncReport {
  applied: number;
  /**
   * Events this build did not understand and skipped. Non-zero is not a
   * fault — it means the server is newer than this client.
   */
  ignored: number;
  pages: number;
  cursor: number;
  /**
   * The pass fell back to a full snapshot, either because it was the
   * first one or because an event could not be applied.
   */
  resnapshotted: boolean;
}

/**
 * Bring the cached remote library up to date, bootstrapping if it never
 * has been.
 *
 * Resolves to an all-zero report when the profile is unbound, signed
 * out, or bound to a server without a journal — all ordinary states, not
 * errors.
 */
export function remoteSyncNow(): Promise<RemoteSyncReport> {
  return invoke<RemoteSyncReport>("remote_sync_now");
}

export interface RemoteOverview {
  playlists: number;
  favorites: number;
  ratings: number;
  history: number;
  shares: number;
  queue_tracks: number;
  cached_tracks: number;
  /**
   * Identifiers the projection references but has no metadata for.
   * Normal right after a catch-up; it should fall back to zero on the
   * next pass.
   */
  tracks_awaiting_metadata: number;
  /** Local changes the server has not accepted yet. */
  pending_changes: number;
  /**
   * Local changes the server refused permanently. These will never be
   * retried, so surface them — nobody else will say anything.
   */
  failed_changes: number;
}

export interface RemotePlaylistSummary {
  id: string;
  name: string;
  comment: string | null;
  is_public: boolean;
  track_count: number;
  /**
   * Sums only the tracks whose metadata is cached, so it understates
   * while a backfill is outstanding. Showing an approximate duration
   * beats refusing to show one.
   */
  duration_ms: number;
  /**
   * Created here and never sent: the server has no such playlist yet.
   * Worth showing, because it is the one that disappears if the user
   * forgets this server.
   */
  pending_creation: boolean;
}

export interface RemoteTrack {
  id: string;
  /** `null` while the metadata has not been fetched yet. */
  title: string | null;
  artist: string | null;
  album: string | null;
  duration_ms: number | null;
  artwork_hash: string | null;
}

/** Counts for the diagnostics panel. Local only — instant, and the same
 * answer whether or not the server is reachable. */
export function remoteGetOverview(): Promise<RemoteOverview> {
  return invoke<RemoteOverview>("remote_get_overview");
}

export function remoteListPlaylists(): Promise<RemotePlaylistSummary[]> {
  return invoke<RemotePlaylistSummary[]>("remote_list_playlists");
}

/**
 * One playlist's tracks, in order. Entries still awaiting metadata come
 * back with a null title rather than being skipped — dropping them would
 * make the playlist look shorter than it is.
 */
export function remoteListPlaylistTracks(
  playlistId: string,
): Promise<RemoteTrack[]> {
  return invoke<RemoteTrack[]>("remote_list_playlist_tracks", { playlistId });
}

export function remoteListQueue(): Promise<RemoteTrack[]> {
  return invoke<RemoteTrack[]>("remote_list_queue");
}
