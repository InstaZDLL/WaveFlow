import { SETTINGS } from "../lib/settingsRegistry";
import { useProfileSetting, type ProfileSetting } from "./useProfileSetting";

/** Its own broadcast channel, as every preference has. */
export const PODCAST_DOWNLOAD_QUOTA_EVENT = "waveflow:podcast-download-quota";

/**
 * A whole number of GiB, at least one. Read the way
 * `podcast::store::download_quota_bytes` reads it, so what this shows is what the
 * backend applies.
 */
export function parsePodcastDownloadQuota(raw: string | null): number {
  const value = Number(raw);
  return raw !== null && Number.isInteger(value) && value >= 1
    ? value
    : SETTINGS["podcasts.download_quota_gb"].defaultValue;
}

/**
 * The most this profile keeps of downloaded episodes, in GiB. The backend reads the
 * key when a download starts and counts against it while the body is read.
 */
export function usePodcastDownloadQuota(): ProfileSetting<number> {
  return useProfileSetting<number>({
    key: "podcasts.download_quota_gb",
    defaultValue: SETTINGS["podcasts.download_quota_gb"].defaultValue,
    parse: parsePodcastDownloadQuota,
    serialize: String,
    valueType: "int",
    event: PODCAST_DOWNLOAD_QUOTA_EVENT,
    label: "usePodcastDownloadQuota",
  });
}
