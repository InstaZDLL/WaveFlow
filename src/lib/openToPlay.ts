import { open } from "@tauri-apps/plugin-dialog";
import i18n from "../i18n";
import { playerPlayPaths } from "./tauri/player";

/**
 * What the player decodes: `waveflow_core::scanner::extract::AUDIO_EXTENSIONS`.
 * The picker offers these; anything else in a folder is passed over by the
 * backend anyway.
 */
export const PLAYABLE_EXTENSIONS = [
  "mp3",
  "flac",
  "wav",
  "aiff",
  "aif",
  "ogg",
  "oga",
  "opus",
  "m4a",
  "mp4",
  "aac",
  "dsf",
  "dff",
];

/**
 * Play files or folders without importing them: the drop's "Play now"
 * target and the two pickers below. Files that all are library tracks play
 * as such; anything else plays as a queue of files (`player_play_paths`).
 */
export async function playWithoutImporting(paths: string[]): Promise<void> {
  if (paths.length === 0) return;
  try {
    await playerPlayPaths(paths);
  } catch (err) {
    console.error("[openToPlay] could not play", err);
  }
}

function asList(result: string | string[] | null): string[] {
  if (result == null) return [];
  return Array.isArray(result) ? result : [result];
}

/** Pick audio files and play them without importing them. */
export async function openFilesToPlay(): Promise<void> {
  const picked = await open({
    multiple: true,
    directory: false,
    title: i18n.t("openToPlay.filesTitle"),
    filters: [
      {
        name: i18n.t("openToPlay.filterName"),
        extensions: PLAYABLE_EXTENSIONS,
      },
    ],
  });
  await playWithoutImporting(asList(picked));
}

/** Pick a folder and play the audio files in it without importing them. */
export async function openFolderToPlay(): Promise<void> {
  const picked = await open({
    multiple: false,
    directory: true,
    title: i18n.t("openToPlay.folderTitle"),
  });
  await playWithoutImporting(asList(picked));
}
