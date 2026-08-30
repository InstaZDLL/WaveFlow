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
  /** Server id of the primary artist, for navigating to a remote artist
   *  view; `null` when the server has no primary artist. */
  artist_id: string | null;
  album: string | null;
  /** Server id of the album, for navigating to a remote album view. */
  album_id: string | null;
  duration_ms: number | null;
  artwork_hash: string | null;
  /** Whether the track is starred, from the synced favorites. */
  starred: boolean;
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

/**
 * Mint a locally-playable stream URL for a projected remote track. The
 * server returns a sealed ticket; this resolves to an absolute URL safe to
 * hand to `playerPlayUrl` (self-authenticating, TTL ~1h, Range-capable).
 */
export function remoteStreamUrl(trackId: string): Promise<string> {
  return invoke<string>("remote_stream_url", { trackId });
}

/** Resolve a remote cover to a **local file path**, downloading it once into
 *  a per-profile disk cache if needed. The artwork endpoint is Bearer-only, so
 *  a bare `<img src>` to it would 401; the path goes through the asset
 *  protocol exactly like a scanned local cover, so `resolveArtwork` handles it
 *  with no special case.
 *
 *  Only hash-addressed covers are cacheable and only those are accepted: the
 *  server keeps its track/album/artist aliases revalidatable because a rescan
 *  can move the cover they resolve to. */
export function remoteArtwork(artworkHash: string): Promise<string> {
  return invoke<string>("remote_artwork", { artworkHash });
}

/**
 * Play a remote playlist as a native queue, starting at `startIndex`. The
 * backend fills an in-memory remote queue from the projection, mints a
 * stream ticket for the first track, and streams it through the engine —
 * so the tracks after it auto-advance and the PlayerBar's next / previous
 * (and the media keys) drive the remote queue while it is playing.
 */
export function remotePlayPlaylist(
  playlistId: string,
  startIndex: number,
): Promise<void> {
  return invoke<void>("remote_play_playlist", { playlistId, startIndex });
}

export interface RemotePlayQueueRow {
  id: string;
  title: string | null;
  artist: string | null;
  /** Server id of the primary artist — lets the "About the artist" panel
   *  link to the remote artist and fetch its photo. */
  artist_id: string | null;
  artwork_hash: string | null;
  duration_ms: number | null;
}

export interface RemotePlayQueue {
  entries: RemotePlayQueueRow[];
  /** Index of the entry currently playing. */
  index: number;
}

export interface RemoteAlbum {
  id: string;
  title: string;
  artist: string | null;
  artist_id: string | null;
  artwork_hash: string | null;
  year: number | null;
  tracks: RemoteTrack[];
}

/**
 * Fetch a remote album with its tracks (`GET /api/v2/albums/{id}`). The
 * songs are cached server-side so they render and play at once.
 */
export function remoteGetAlbum(albumId: string): Promise<RemoteAlbum> {
  return invoke<RemoteAlbum>("remote_get_album", { albumId });
}

export interface RemoteAlbumSummary {
  id: string;
  title: string;
  artist: string | null;
  artwork_hash: string | null;
  year: number | null;
}

export interface RemoteArtist {
  id: string;
  name: string;
  artwork_hash: string | null;
  albums: RemoteAlbumSummary[];
}

/**
 * Fetch a remote artist with their albums (`GET /api/v2/artists/{id}`). The
 * server carries the artist image; the biography comes from Last.fm by name.
 */
export function remoteGetArtist(artistId: string): Promise<RemoteArtist> {
  return invoke<RemoteArtist>("remote_get_artist", { artistId });
}

/**
 * Play an explicit list of remote track ids as a native queue from
 * `startIndex` — used to play an album. Metadata comes from the cache.
 */
export function remotePlayTracks(
  trackIds: string[],
  startIndex: number,
): Promise<void> {
  return invoke<void>("remote_play_tracks", { trackIds, startIndex });
}

/**
 * Snapshot the live remote play queue, or `null` when the current
 * playback is a library track or a radio stream (i.e. no remote session).
 * Read from memory — instant, no server round-trip.
 */
export function remoteGetPlayQueue(): Promise<RemotePlayQueue | null> {
  return invoke<RemotePlayQueue | null>("remote_get_play_queue");
}

/** Jump the remote play queue to an absolute position and play it. */
export function remoteQueueJump(index: number): Promise<void> {
  return invoke<void>("remote_queue_jump", { index });
}

/**
 * Local gestures on remote data.
 *
 * Each resolves as soon as the change is durable **locally** — it is
 * written to the cached library and queued for the server in one
 * transaction — not when the server has acknowledged it. That is
 * deliberate: the change is not at risk, so there is nothing for the
 * user to wait on, and the same call works offline.
 *
 * The server echoes each one back through the journal, which is
 * harmless: applying our own change twice lands on the same state.
 */

/** Star or unstar a remote track, album or artist. */
export function remoteSetFavorite(
  entityType: string,
  entityId: string,
  starred: boolean,
): Promise<void> {
  return invoke<void>("remote_set_favorite", {
    entityType,
    entityId,
    starred,
  });
}

/** Rate from 1 to 5, or pass `0` to clear the rating. */
export function remoteSetRating(
  entityType: string,
  entityId: string,
  rating: number,
): Promise<void> {
  return invoke<void>("remote_set_rating", { entityType, entityId, rating });
}

/**
 * Create a remote playlist.
 *
 * Resolves to the identifier it is known by locally — a temporary one
 * until the creation reaches the server, which then replaces it. Re-read
 * rather than holding on to it.
 */
export function remoteCreatePlaylist(
  name: string,
  trackIds: string[] = [],
): Promise<string> {
  return invoke<string>("remote_create_playlist", { name, trackIds });
}

/**
 * Rename a playlist, set or empty its comment, change its visibility.
 *
 * Omitting a field leaves it untouched. Emptying the comment needs
 * `clearComment` — the server treats an absent value and an explicit
 * null identically, so naming the field is the only way to say "empty".
 */
export function remoteUpdatePlaylist(
  playlistId: string,
  changes: {
    name?: string;
    comment?: string;
    public?: boolean;
    clearComment?: boolean;
  },
): Promise<void> {
  return invoke<void>("remote_update_playlist", {
    playlistId,
    name: changes.name ?? null,
    comment: changes.comment ?? null,
    public: changes.public ?? null,
    clearComment: changes.clearComment ?? false,
  });
}

export function remoteDeletePlaylist(playlistId: string): Promise<void> {
  return invoke<void>("remote_delete_playlist", { playlistId });
}

/**
 * Remove the track at `index` (its position in the current order) from a
 * remote playlist. Applies locally at once and queues the change for the
 * server, like the other remote gestures.
 */
export function remoteRemovePlaylistTrack(
  playlistId: string,
  index: number,
): Promise<void> {
  return invoke<void>("remote_remove_playlist_track", { playlistId, index });
}

/**
 * Move the track at `from` to `to` within a remote playlist (positions in
 * the current order). Applies locally at once and queues the new order for
 * the server.
 */
export function remoteReorderPlaylistTrack(
  playlistId: string,
  from: number,
  to: number,
): Promise<void> {
  return invoke<void>("remote_reorder_playlist_track", {
    playlistId,
    from,
    to,
  });
}

/**
 * Search the remote server's catalogue for tracks. A live query capped at a
 * page; each hit's metadata is cached server-side so adding it renders a
 * title at once. Empty for a blank query.
 */
/**
 * What the bound server's transcoder can do, and how busy it is.
 *
 * `available` is a startup capability — the server found both FFmpeg tools —
 * so a `false` here means the preference cannot be honoured however it is
 * set. The two ceilings are what a `429` on the stream route enforces.
 */
export interface RemoteTranscodeStatus {
  available: boolean;
  active: number;
  global_limit: number;
  per_user_limit: number;
}

export function remoteTranscodeStatus(): Promise<RemoteTranscodeStatus> {
  return invoke<RemoteTranscodeStatus>("remote_transcode_status");
}

/**
 * Disk held by cached remote audio, counted apart from the covers: whole
 * songs and thumbnails differ in size by two orders of magnitude, and one
 * figure covering both would be read as the smaller one.
 */
export interface StreamCacheInfo {
  bytes: number;
  tracks: number;
}

export function remoteStreamCacheInfo(): Promise<StreamCacheInfo> {
  return invoke<StreamCacheInfo>("remote_stream_cache_info");
}

/**
 * An offline copy of a server track, kept in a folder the scanner never sees.
 *
 * Not a local library track: it describes a *remote* one that happens to be on
 * this disk, which is why it is keyed by the server's id.
 */
export interface DownloadedTrack {
  remote_track_id: string;
  path: string;
  /** BLAKE3 of the whole file, computed while writing it — the server's own
   *  digest, so directly comparable, unlike the library's `file_hash`. */
  full_hash: string;
  size: number;
  downloaded_at: number;
}

export interface DownloadsInfo {
  bytes: number;
  tracks: number;
}

/** Progress event payload (`remote:download-progress`), one per megabyte. */
export interface DownloadProgress {
  track_id: string;
  received: number;
  total: number | null;
}

/** Keep a track's original bytes. Answering with the existing copy when there
 *  already is one, so asking twice costs nothing. */
export function remoteDownloadTrack(trackId: string): Promise<DownloadedTrack> {
  return invoke<DownloadedTrack>("remote_download_track", { trackId });
}

export function remoteListDownloads(): Promise<DownloadedTrack[]> {
  return invoke<DownloadedTrack[]>("remote_list_downloads");
}

export function remoteDownloadsInfo(): Promise<DownloadsInfo> {
  return invoke<DownloadsInfo>("remote_downloads_info");
}

/** Drop one offline copy. `false` when there was none. */
export function remoteRemoveDownload(trackId: string): Promise<boolean> {
  return invoke<boolean>("remote_remove_download", { trackId });
}

/** Drop every offline copy. Returns how many were removed. */
export function remoteClearDownloads(): Promise<number> {
  return invoke<number>("remote_clear_downloads");
}

/**
 * A scanned folder an import can land in.
 *
 * `exists` is false when the path is not on this machine right now — an
 * unplugged drive, a share that is down. Still listed, because saying so is
 * more useful than quietly dropping the destination someone always uses.
 */
export interface ImportFolder {
  folder_id: number;
  library_id: number;
  path: string;
  exists: boolean;
}

/** Why one track was not copied. Named rather than boolean: `already_linked`
 *  and `already_held` both mean "you have this", `unsupported_format` will
 *  never work, and `failed` is worth retrying. */
export type ImportRefusal =
  | "already_linked"
  | "already_held"
  | "unknown_track"
  | "unsupported_format"
  | "hash_mismatch"
  | "not_indexed"
  | "failed";

export interface ImportedTrack {
  remote_track_id: string;
  local_track_id: number;
  path: string;
  full_hash: string;
}

export interface SkippedImport {
  remote_track_id: string;
  reason: ImportRefusal;
  local_track_id?: number;
}

export interface ImportOutcome {
  imported: ImportedTrack[];
  skipped: SkippedImport[];
}

/** Progress event payload (`remote:import-progress`), one per megabyte.
 *  Separate from a download's: same bytes, different feature. */
export type ImportProgress = DownloadProgress;

/** The scanned folders an import can target. */
export function remoteImportFolders(): Promise<ImportFolder[]> {
  return invoke<ImportFolder[]>("remote_import_folders");
}

/**
 * Copy server tracks into a scanned folder, index them, and link each one back
 * to the track it came from.
 *
 * The folder is scanned once at the end, so `scan:progress` fires as well as
 * `remote:import-progress`.
 */
export function remoteImportTracks(
  trackIds: string[],
  folderId: number,
): Promise<ImportOutcome> {
  return invoke<ImportOutcome>("remote_import_tracks", { trackIds, folderId });
}

/** Drop every cached stream. Costs one download per track played again. */
export function remoteClearStreamCache(): Promise<number> {
  return invoke<number>("remote_clear_stream_cache");
}

export function remoteSearchCatalogue(query: string): Promise<RemoteTrack[]> {
  return invoke<RemoteTrack[]>("remote_search_catalogue", { query });
}

/** Append tracks to a remote playlist. Applies locally at once and queues
 *  the additions for the server. */
export function remoteAddPlaylistTracks(
  playlistId: string,
  trackIds: string[],
): Promise<void> {
  return invoke<void>("remote_add_playlist_tracks", { playlistId, trackIds });
}

/**
 * Record a play against the remote account. `submission: false` is a
 * "now playing" ping; only a completed listen enters the history.
 *
 * **`trackId` must be a server identifier.** Nothing calls this from
 * playback, and nothing should until remote playback exists: the local
 * player deals in file paths and local row ids, which the server
 * validates and rejects — every such call would fail permanently and
 * pile up in the outbound queue.
 */
export function remoteScrobble(
  trackId: string,
  submission: boolean,
  playedAt?: number,
): Promise<void> {
  return invoke<void>("remote_scrobble", {
    trackId,
    submission,
    playedAt: playedAt ?? null,
  });
}

/** Save the account's play queue. Same identifier caveat as
 * {@link remoteScrobble}. */
export function remoteSaveQueue(
  trackIds: string[],
  current: string | null,
  positionMs: number,
  client?: string,
): Promise<void> {
  return invoke<void>("remote_save_queue", {
    trackIds,
    current,
    positionMs,
    client: client ?? null,
  });
}

/**
 * Publish a share of remote tracks.
 *
 * Resolves to the local identifier. The public link is **not** available
 * yet — the token is derived from a server-side secret, so it only
 * arrives with the server's response. A share created offline has no
 * link to copy until it lands.
 */
export function remoteCreateShare(
  trackIds: string[],
  description?: string,
  expiresAt?: number,
): Promise<string> {
  return invoke<string>("remote_create_share", {
    trackIds,
    description: description ?? null,
    expiresAt: expiresAt ?? null,
  });
}

/**
 * Change a share's description or expiry, or empty either.
 *
 * The `clear*` flags are the only way to empty a field: omitting it
 * leaves it in place, so an expiry set by mistake would otherwise be
 * permanent — the owner's only recourse being to withdraw the share and
 * publish a different link.
 */
export function remoteUpdateShare(
  shareId: string,
  changes: {
    description?: string;
    expiresAt?: number;
    clearDescription?: boolean;
    clearExpiresAt?: boolean;
  },
): Promise<void> {
  return invoke<void>("remote_update_share", {
    shareId,
    description: changes.description ?? null,
    expiresAt: changes.expiresAt ?? null,
    clearDescription: changes.clearDescription ?? false,
    clearExpiresAt: changes.clearExpiresAt ?? false,
  });
}

export function remoteDeleteShare(shareId: string): Promise<void> {
  return invoke<void>("remote_delete_share", { shareId });
}

export interface LocalMatchCandidate {
  track_id: number;
  title: string;
  artist: string | null;
  album: string | null;
  file_path: string;
  size: number;
}

export interface RemoteMatchCandidate {
  track_id: string;
  title: string;
  artist: string | null;
  album: string | null;
  size: number;
}

export interface MatchCandidateGroup {
  full_hash: string;
  local_tracks: LocalMatchCandidate[];
  remote_tracks: RemoteMatchCandidate[];
}

export interface ReconciliationReport {
  hashed_local_tracks: number;
  unreadable_local_tracks: number;
  auto_linked: number;
  verified_links: number;
  stale_links: number;
  rejected_pairs: number;
  /** `true` when the user cancelled mid-scan; the report is otherwise empty. */
  cancelled: boolean;
  /** `true` when another scan already owns the run; ignore this report and keep
   * the candidates already on screen rather than clearing them. */
  already_running: boolean;
  candidates: MatchCandidateGroup[];
}

/** Progress payload emitted on `reconcile:progress` while a scan hashes files. */
export interface ReconcileProgress {
  processed: number;
  total: number;
}

export interface ReconciliationLink {
  local_track_id: number;
  remote_track_id: string;
  local_title: string;
  remote_title: string | null;
  method: "exact_full_hash" | "confirmed_mbid";
  verified_full_hash: string | null;
  status: "confirmed" | "stale";
  playback_preference: "local_first" | "server_first";
  confirmed_at: number;
  verified_at: number;
  local_favorite: boolean;
  remote_favorite: boolean;
  local_rating: number | null;
  remote_rating: number | null;
  local_plays: number;
  remote_plays: number;
  combined_plays: number;
}

export type PlaylistConversionDirection = "local_to_server" | "server_to_local";

export interface PlaylistConversionItem {
  position: number;
  title: string;
  local_track_id: number | null;
  remote_track_id: string | null;
  status: "confirmed" | "stale" | "unlinked_or_ambiguous" | "duplicate";
}

export interface PlaylistConversionPreview {
  direction: PlaylistConversionDirection;
  source_id: string;
  source_name: string;
  total_tracks: number;
  convertible_tracks: number;
  blocked_tracks: number;
  can_convert: boolean;
  items: PlaylistConversionItem[];
}

export interface PlaylistConversionResult {
  direction: PlaylistConversionDirection;
  destination_id: string;
  converted_tracks: number;
}

/** What the remote cover cache holds on disk. */
export interface ArtworkCacheInfo {
  bytes: number;
  covers: number;
}

export function remoteArtworkCacheInfo(): Promise<ArtworkCacheInfo> {
  return invoke<ArtworkCacheInfo>("remote_artwork_cache_info");
}

/** Delete every cached remote cover. Costs one download each time one is
 *  looked at again — the images are content-addressed, so nothing is lost. */
export function remoteClearArtworkCache(): Promise<void> {
  return invoke<void>("remote_clear_artwork_cache");
}

/** What one catalogue walk did. Counts are of work performed, so all-zero on a
 * populated server means "nothing had changed", not "nothing was found". */
export interface CatalogueMirrorReport {
  albums_seen: number;
  albums_walked: number;
  tracks_mirrored: number;
  orphans_mirrored: number;
  removed: number;
  libraries: number;
  cancelled: boolean;
  already_running: boolean;
}

/** Progress emitted on `remote:mirror-progress`. `total` is 0 while a phase is
 * still counting, so render it as indeterminate rather than as 0 %. */
export interface CatalogueMirrorProgress {
  phase: "albums" | "sweep";
  done: number;
  total: number;
}

/** What the mirror currently holds. */
export interface CatalogueStats {
  albums: number;
  /** Albums whose tracks have been walked; below `albums` while a walk is
   * still in progress or was cancelled. */
  albums_mirrored: number;
  tracks: number;
  artists: number;
  libraries: number;
  /** Oldest library sweep, or `null` until every library has been swept once —
   * a partial mirror must not show a date that reads as "up to date". */
  mirrored_at: number | null;
}

/**
 * Walk the server's catalogue into the local projection so both sources can be
 * browsed from one library. Incremental: an album whose track count is
 * unchanged is not re-fetched.
 */
export function remoteMirrorCatalogue(): Promise<CatalogueMirrorReport> {
  return invoke<CatalogueMirrorReport>("remote_mirror_catalogue");
}

/** Ask an in-flight walk to stop; resolves to whether one was running. What was
 * already committed stays, and the next walk resumes from what is missing. */
export function remoteCancelCatalogueMirror(): Promise<boolean> {
  return invoke<boolean>("remote_cancel_catalogue_mirror");
}

export function remoteCatalogueStats(): Promise<CatalogueStats> {
  return invoke<CatalogueStats>("remote_catalogue_stats");
}

/** Drop the mirrored catalogue. Rows the user data still references are kept. */
export function remoteClearCatalogue(): Promise<void> {
  return invoke<void>("remote_clear_catalogue");
}

/**
 * Find local/server identity links. The backend hashes only local files whose
 * byte size exists in the remote cache; unique exact matches are persisted,
 * while duplicate groups come back for explicit confirmation.
 */
export function remoteReconcileScan(): Promise<ReconciliationReport> {
  return invoke<ReconciliationReport>("remote_reconcile_scan");
}

/** Ask an in-flight scan to stop; resolves to whether one was running. */
export function remoteCancelReconcileScan(): Promise<boolean> {
  return invoke<boolean>("remote_cancel_reconcile_scan");
}

export function remoteListReconciliationLinks(): Promise<ReconciliationLink[]> {
  return invoke<ReconciliationLink[]>("remote_list_reconciliation_links");
}

export function remoteConfirmReconciliation(
  localTrackId: number,
  remoteTrackId: string,
): Promise<void> {
  return invoke<void>("remote_confirm_reconciliation", {
    localTrackId,
    remoteTrackId,
  });
}

export function remoteRejectReconciliation(
  localTrackId: number,
  remoteTrackId: string,
): Promise<void> {
  return invoke<void>("remote_reject_reconciliation", {
    localTrackId,
    remoteTrackId,
  });
}

export function remoteSetReconciliationPreference(
  localTrackId: number,
  preference: "local_first" | "server_first",
): Promise<void> {
  return invoke<void>("remote_set_reconciliation_preference", {
    localTrackId,
    preference,
  });
}

export function remoteRemoveReconciliationLink(
  localTrackId: number,
): Promise<void> {
  return invoke<void>("remote_remove_reconciliation_link", { localTrackId });
}

export function remoteCopyReconciliationFavorite(
  localTrackId: number,
  direction: PlaylistConversionDirection,
): Promise<void> {
  return invoke<void>("remote_copy_reconciliation_favorite", {
    localTrackId,
    direction,
  });
}

export function remoteCopyReconciliationRating(
  localTrackId: number,
  direction: PlaylistConversionDirection,
): Promise<void> {
  return invoke<void>("remote_copy_reconciliation_rating", {
    localTrackId,
    direction,
  });
}

export function remotePreviewPlaylistConversion(
  direction: PlaylistConversionDirection,
  sourceId: string,
): Promise<PlaylistConversionPreview> {
  return invoke<PlaylistConversionPreview>(
    "remote_preview_playlist_conversion",
    { direction, sourceId },
  );
}

export function remoteConvertPlaylist(
  direction: PlaylistConversionDirection,
  sourceId: string,
): Promise<PlaylistConversionResult> {
  return invoke<PlaylistConversionResult>("remote_convert_playlist", {
    direction,
    sourceId,
  });
}
