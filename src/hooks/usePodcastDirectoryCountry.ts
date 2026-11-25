import { useMemo } from "react";

import { detectLocalRegion } from "../lib/region";
import { isPodcastStorefront } from "../lib/podcastDirectory";
import { useProfileSetting } from "./useProfileSetting";

/** Its own broadcast channel, as every preference has. */
export const PODCAST_DIRECTORY_COUNTRY_EVENT =
  "waveflow:podcast-directory-country";

/** Not shown in Settings: the directory's own picker is where it is chosen, so
 *  it has no entry in the registry. Empty is "where the user seems to be". */
const NOT_CHOSEN = "";

function parse(raw: string | null): string {
  const code = (raw ?? "").trim().toUpperCase();
  return isPodcastStorefront(code) ? code : NOT_CHOSEN;
}

function serialize(code: string): string {
  return code;
}

/** Where the directory starts when nothing was chosen: the region the time
 *  zone or the language says, when the directory has a storefront there. */
function guessed(): string {
  const region = detectLocalRegion();
  return region && isPodcastStorefront(region) ? region : "US";
}

/**
 * The country whose charts and storefront the podcast directory shows, two
 * upper-case letters, per profile. Until one is picked, the one the user seems
 * to be in.
 */
export function usePodcastDirectoryCountry(): {
  country: string;
  setCountry: (code: string) => void;
  ready: boolean;
} {
  const setting = useProfileSetting<string>({
    key: "podcasts.directory_country",
    defaultValue: NOT_CHOSEN,
    parse,
    serialize,
    valueType: "string",
    event: PODCAST_DIRECTORY_COUNTRY_EVENT,
    label: "usePodcastDirectoryCountry",
  });
  const fallback = useMemo(() => guessed(), []);
  return {
    country: setting.value || fallback,
    setCountry: setting.setValue,
    ready: setting.ready,
  };
}
