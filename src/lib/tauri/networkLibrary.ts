import { invoke } from "@tauri-apps/api/core";

/** Broadcast after the cache budget is written. */
export const NETWORK_CACHE_LIMIT_EVENT = "waveflow:network-cache-limit";

/** What the form sends. The password goes one way: nothing returns it. */
export interface NetworkSourceInput {
  kind: NetworkSourceKind;
  name: string;
  endpoint: string;
  share: string;
  root: string;
  username: string | null;
  password: string | null;
  allowPlainHttp: boolean;
}

export type NetworkSourceKind = "webdav" | "smb";

export type NetworkSourceState =
  "unknown" | "online" | "unreachable" | "auth_failed";

export interface NetworkSourceView {
  id: number;
  uuid: string;
  kind: NetworkSourceKind;
  name: string;
  endpoint: string;
  share: string;
  username: string | null;
  hasPassword: boolean;
  plainHttp: boolean;
  state: NetworkSourceState;
  lastError: string | null;
  folders: { id: number; path: string }[];
}

export type NetworkFailureCode =
  | "offline"
  | "unreachable"
  | "handshake"
  | "tls"
  | "auth"
  | "session-refused"
  | "not-found"
  | "protocol"
  | "plain-http";

export interface NetworkTestOutcome {
  ok: boolean;
  code: NetworkFailureCode | null;
  /** The cause as reported, technical and untranslated. */
  detail: string | null;
  entries: number | null;
}

export interface NetworkAddOutcome {
  test: NetworkTestOutcome;
  folderId: number | null;
}

export function testNetworkSource(
  input: NetworkSourceInput,
): Promise<NetworkTestOutcome> {
  return invoke<NetworkTestOutcome>("network_source_test", { input });
}

export function addNetworkSource(
  libraryId: number,
  input: NetworkSourceInput,
): Promise<NetworkAddOutcome> {
  return invoke<NetworkAddOutcome>("network_source_add", { libraryId, input });
}

export function listNetworkSources(): Promise<NetworkSourceView[]> {
  return invoke<NetworkSourceView[]>("network_source_list");
}

export function setNetworkSourceCredentials(
  id: number,
  username: string | null,
  password: string | null,
): Promise<void> {
  return invoke<void>("network_source_set_credentials", {
    id,
    username,
    password,
  });
}

export function removeNetworkSource(id: number): Promise<void> {
  return invoke<void>("network_source_remove", { id });
}

export function getNetworkBlockedTracks(): Promise<number[]> {
  return invoke<number[]>("network_blocked_tracks");
}

export function getNetworkCacheInfo(): Promise<{
  bytes: number;
  entries: number;
}> {
  return invoke("network_cache_info");
}

export function clearNetworkCache(): Promise<number> {
  return invoke<number>("network_cache_clear");
}

export function notifyNetworkCacheLimitChanged(): Promise<void> {
  return invoke<void>("network_cache_limit_changed");
}
