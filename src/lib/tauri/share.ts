import { invoke } from "@tauri-apps/api/core";

/**
 * Persist a frontend-rendered PNG at `targetPath`. `bytes` is expected
 * to be a raw PNG byte stream — the backend writes it verbatim, so any
 * image-encoder roundtrip happens upstream. Shared by Wrapped + Now
 * Playing share cards.
 */
export function saveShareImage(
  bytes: Uint8Array,
  targetPath: string,
): Promise<void> {
  // Tauri's `invoke` serialises Uint8Array as a JSON number array; the
  // Rust side receives `Vec<u8>`. This is the canonical pattern in the
  // tauri-apps docs for the IPC byte channel.
  return invoke<void>("save_share_image", {
    bytes: Array.from(bytes),
    targetPath,
  });
}

// ── Public share links (Phase 1.g.3-desktop) ─────────────────────

export interface ShareLink {
  token: string;
  url: string;
}

export interface ShareStatus {
  link: ShareLink | null;
}

/**
 * Mint a public share link for the playlist. Idempotent — calling
 * twice returns the same token. The link stays usable until
 * [`shareLinkRevoke`] is called.
 */
export function shareLinkMint(playlistId: number): Promise<ShareLink> {
  return invoke<ShareLink>("share_link_mint", { playlistId });
}

/**
 * Revoke the public share link. After this resolves, the previously-
 * minted token returns 404 on every device. Idempotent.
 */
export function shareLinkRevoke(playlistId: number): Promise<void> {
  return invoke<void>("share_link_revoke", { playlistId });
}

/**
 * Local-only read of the last cached share state for this playlist.
 * No network round-trip — used by the modal to render the initial
 * "active / inactive" state instantly.
 */
export function shareLinkStatus(playlistId: number): Promise<ShareStatus> {
  return invoke<ShareStatus>("share_link_status", { playlistId });
}
