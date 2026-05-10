import { invoke } from "@tauri-apps/api/core";

export interface ChangelogEntry {
  hash: string;
  type: string;
  scope: string | null;
  subject: string;
  breaking: boolean;
  /** ISO-8601 committer date. */
  date: string;
}

export function getChangelog(): Promise<ChangelogEntry[]> {
  return invoke<ChangelogEntry[]>("get_changelog");
}
