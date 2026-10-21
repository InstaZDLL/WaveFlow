import { emit } from "@tauri-apps/api/event";
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
 * Say a failure through the playback alert, as the engine's own are said:
 * the drop, the menu and the shortcuts have nothing else on screen to show
 * it, and a folder with no audio file in it would otherwise do nothing at
 * all. The message stays in the console and in the alert's tooltip.
 */
function report(kind: "files-failed" | "picker-failed", err: unknown): void {
  console.error(`[openToPlay] ${kind}`, err);
  void emit("player:error", { kind, message: String(err) }).catch(() => {});
}

/**
 * Play files or folders without importing them: the drop's "Play now"
 * target and the two pickers below. Files that all are library tracks play
 * as such; anything else plays as a queue of files (`player_play_paths`).
 * Never rejects: a failure is reported to the user.
 */
export async function playWithoutImporting(paths: string[]): Promise<void> {
  if (paths.length === 0) return;
  try {
    await playerPlayPaths(paths);
  } catch (err) {
    report("files-failed", err);
  }
}

/**
 * The picker's answer as a list: empty when it was cancelled, and when it
 * failed, the failure then reported.
 */
async function pick(options: Parameters<typeof open>[0]): Promise<string[]> {
  try {
    return asList(await open(options));
  } catch (err) {
    report("picker-failed", err);
    return [];
  }
}

function asList(result: string | string[] | null): string[] {
  if (result == null) return [];
  return Array.isArray(result) ? result : [result];
}

/** Pick audio files and play them without importing them. */
export async function openFilesToPlay(): Promise<void> {
  const picked = await pick({
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
  await playWithoutImporting(picked);
}

/** Pick a folder and play the audio files in it without importing them. */
export async function openFolderToPlay(): Promise<void> {
  const picked = await pick({
    multiple: false,
    directory: true,
    title: i18n.t("openToPlay.folderTitle"),
  });
  await playWithoutImporting(picked);
}
