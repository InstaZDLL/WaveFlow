import { useCallback } from "react";
import type { LyricsProvider } from "../lib/tauri/lyrics";
import { useProfileSetting } from "./useProfileSetting";

/**
 * The online lyrics providers the user can switch off (#722), in the
 * order the backend asks them. Mirrors `lyrics_providers::is_switchable`:
 * LRCLIB is the backbone of the lookup and stays on, Musixmatch has its
 * own opt-in.
 */
export const SWITCHABLE_LYRICS_PROVIDERS: readonly LyricsProvider[] = [
  "net_ease",
  "megalobiz",
  "genius",
] as const;

/** Mirrors `DEFAULT_DISABLED` in `lyrics_providers.rs`: Genius answers a
 *  normal install with a 403, and only ever with unsynced text. */
const DEFAULT_DISABLED: LyricsProvider[] = ["genius"];

/** Mirrors `DEFAULT_EXCLUDED_GENRES` in `lyrics_providers.rs`. */
export const DEFAULT_EXCLUDED_GENRES: string[] = ["Instrumental", "Lo-fi"];

const DISABLED_EVENT = "waveflow:lyrics-disabled-providers-changed";
const GENRES_EVENT = "waveflow:lyrics-excluded-genres-changed";

/**
 * Parse a stored JSON array of strings. Junk falls back to `fallback`,
 * as the backend does — an unreadable genre list must not read as
 * "exclude nothing" here while the backend keeps excluding.
 */
function parseStringList(raw: string | null, fallback: string[]): string[] {
  if (raw == null) return fallback;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return fallback;
    return parsed.filter((v): v is string => typeof v === "string");
  } catch {
    return fallback;
  }
}

function parseDisabled(raw: string | null): LyricsProvider[] {
  return parseStringList(raw, DEFAULT_DISABLED).filter(
    (id): id is LyricsProvider =>
      SWITCHABLE_LYRICS_PROVIDERS.includes(id as LyricsProvider),
  );
}

function parseGenres(raw: string | null): string[] {
  return parseStringList(raw, DEFAULT_EXCLUDED_GENRES);
}

/**
 * Per-profile switches for the online lyrics providers, backed by
 * `profile_setting['lyrics.disabled_providers']` (JSON array of the ids
 * switched OFF). The backend reads the same row before every lookup.
 */
export function useDisabledLyricsProviders() {
  const { value, ready, setValue } = useProfileSetting<LyricsProvider[]>({
    key: "lyrics.disabled_providers",
    defaultValue: DEFAULT_DISABLED,
    parse: parseDisabled,
    // Declaration order, so the stored blob is stable.
    serialize: (ids) =>
      JSON.stringify(
        SWITCHABLE_LYRICS_PROVIDERS.filter((p) => ids.includes(p)),
      ),
    valueType: "json",
    event: DISABLED_EVENT,
    label: "useDisabledLyricsProviders",
  });

  const setEnabled = useCallback(
    (provider: LyricsProvider, enabled: boolean) => {
      void setValue((previous) =>
        enabled
          ? previous.filter((p) => p !== provider)
          : previous.includes(provider)
            ? previous
            : [...previous, provider],
      );
    },
    [setValue],
  );

  return { disabled: value, ready, setEnabled };
}

/**
 * The words of a genre, lowercased, with every separator dropped — the
 * backend's `words` in `lyrics_providers.rs`. Used here only to refuse a
 * duplicate: `Lo Fi` is already on a list holding `lo-fi`.
 */
function genreKey(genre: string): string {
  return genre
    .split(/[^\p{L}\p{N}]+/u)
    .filter(Boolean)
    .join("")
    .toLowerCase();
}

/**
 * Per-profile list of genres whose tracks skip the online lyrics search
 * (#721), backed by `profile_setting['lyrics.excluded_genres']`. The
 * backend matches variants (`Lofi`, `lo-fi hip hop`), so the list holds
 * patterns, not every spelling.
 */
export function useExcludedLyricsGenres() {
  const { value, ready, setValue } = useProfileSetting<string[]>({
    key: "lyrics.excluded_genres",
    defaultValue: DEFAULT_EXCLUDED_GENRES,
    parse: parseGenres,
    serialize: (genres) => JSON.stringify(genres),
    valueType: "json",
    event: GENRES_EVENT,
    label: "useExcludedLyricsGenres",
  });

  /** Adds `genre` unless it is blank or already listed; returns whether
   *  it was added, so the input knows to clear itself. */
  const add = useCallback(
    (genre: string): boolean => {
      const trimmed = genre.trim();
      const key = genreKey(trimmed);
      if (!key || value.some((g) => genreKey(g) === key)) return false;
      void setValue((previous) =>
        previous.some((g) => genreKey(g) === key)
          ? previous
          : [...previous, trimmed],
      );
      return true;
    },
    [setValue, value],
  );

  const remove = useCallback(
    (genre: string) => {
      void setValue((previous) => previous.filter((g) => g !== genre));
    },
    [setValue],
  );

  const reset = useCallback(() => {
    void setValue(DEFAULT_EXCLUDED_GENRES);
  }, [setValue]);

  return { genres: value, ready, add, remove, reset };
}
