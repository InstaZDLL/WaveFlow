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
