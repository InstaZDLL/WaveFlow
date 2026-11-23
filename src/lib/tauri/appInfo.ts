import { invoke } from "./invoke";

/** The version of the SQLite library the backend runs on, as it reports it. */
export function getSqliteVersion(): Promise<string> {
  return invoke<string>("get_sqlite_version");
}
