import { invoke } from "@tauri-apps/api/core";

/**
 * Keep hand-set Canvas clips and animated covers next to the music
 * (issue #695), in the folder the scanner reserves inside each library
 * root, rather than in the app's data directory.
 *
 * Reading is not governed by this: both locations are hash-addressed and
 * the backend looks in the library first, so a clip keeps playing from
 * wherever it actually is. The setting decides where the *next* one is
 * written.
 */
export function getClipsInLibrary(): Promise<boolean> {
  return invoke<boolean>("get_clips_in_library");
}

export function setClipsInLibrary(enabled: boolean): Promise<void> {
  return invoke<void>("set_clips_in_library", { enabled });
}
