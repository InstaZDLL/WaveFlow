import { useEffect, useState } from "react";
import { Check, Loader2, Split } from "lucide-react";
import {
  inventoryPhantomArtists,
  type PhantomArtist,
} from "../../../lib/tauri/inventory";
import { splitArtist } from "../../../lib/tauri/artistOverrides";
import { useProfileSetting } from "../../../hooks/useProfileSetting";
import { useProfile } from "../../../hooks/useProfile";

/** See the note in `TrackTableHeader`. */
type Translator = (key: string, options?: Record<string, unknown>) => string;

const DISMISSED_KEY = "inventory.dismissed_phantoms";
const DISMISSED_EVENT = "waveflow:inventory-dismissed-phantoms-changed";
const NONE: string[] = [];

function parseDismissed(raw: string | null): string[] {
  if (raw == null) return NONE;
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? parsed.filter((v): v is string => typeof v === "string")
      : NONE;
  } catch {
    return NONE;
  }
}

interface PhantomArtistListProps {
  /** Bumped by the parent whenever the library changes underneath. */
  refreshKey: number;
  /** Called after a split or a dismissal, so the counts re-run. */
  onChanged: () => void;
  t: Translator;
}

/**
 * The "artists to split" category of the inventory (#719): artists whose
 * name looks like several joined by commas, each with what the split
 * would produce and the action inline — no detour through the artist
 * page.
 *
 * A comma is only a hint (`Tyler, The Creator`), so every row also offers
 * "don't split", remembered per profile. The fragments already in the
 * library are marked: they are the evidence, and the rows the split will
 * reuse.
 *
 * Splitting takes a second click to confirm. It relinks every track and
 * deletes the joined artist, and a list invites going down it quickly.
 */
export function PhantomArtistList({
  refreshKey,
  onChanged,
  t,
}: PhantomArtistListProps) {
  // Artist ids belong to one profile's database. The rows carry the
  // profile they were read from, and neither action runs against another:
  // after a switch, a split on a stale row would split whichever artist
  // holds that id in the new profile.
  const profileId = useProfile().activeProfile?.id ?? null;
  const [loaded, setLoaded] = useState<{
    profileId: number | null;
    rows: PhantomArtist[];
  }>({ profileId: null, rows: [] });
  const current = loaded.profileId === profileId;
  const rows = current ? loaded.rows : [];
  const setRows = (update: (prev: PhantomArtist[]) => PhantomArtist[]) =>
    setLoaded((prev) => ({ ...prev, rows: update(prev.rows) }));
  const [loading, setLoading] = useState(true);
  const [armed, setArmed] = useState<number | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const dismissed = useProfileSetting<string[]>({
    key: DISMISSED_KEY,
    defaultValue: NONE,
    parse: parseDismissed,
    serialize: (names) => JSON.stringify(names),
    valueType: "json",
    event: DISMISSED_EVENT,
    label: "PhantomArtistList",
  });

  useEffect(() => {
    let cancelled = false;
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    inventoryPhantomArtists(profileId)
      .then((list) => {
        if (!cancelled) setLoaded({ profileId, rows: list });
      })
      .catch((err) => {
        if (!cancelled) console.error("[PhantomArtistList] load failed", err);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [refreshKey, dismissed.value, profileId]);

  // A confirmation armed in one profile must not carry into the next,
  // where the same id can be another artist, nor an error with it.
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect */
    setArmed(null);
    setError(null);
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [profileId]);

  // An armed "confirm" disarms itself, so a stray click minutes later
  // cannot split an artist the user has long stopped looking at.
  useEffect(() => {
    if (armed == null) return;
    const timer = window.setTimeout(() => setArmed(null), 4000);
    return () => window.clearTimeout(timer);
  }, [armed]);

  const split = async (artist: PhantomArtist) => {
    if (!current) return;
    if (armed !== artist.id) {
      setArmed(artist.id);
      return;
    }
    setArmed(null);
    setBusy(artist.id);
    setError(null);
    try {
      await splitArtist(artist.id, loaded.profileId);
      setRows((prev) => prev.filter((r) => r.id !== artist.id));
      onChanged();
    } catch (err) {
      console.error("[PhantomArtistList] split failed", err);
      setError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const dismiss = (artist: PhantomArtist) => {
    if (!current) return;
    setRows((prev) => prev.filter((r) => r.id !== artist.id));
    void dismissed
      .setValue((prev) =>
        prev.includes(artist.canonical_name)
          ? prev
          : [...prev, artist.canonical_name],
      )
      .then(onChanged);
  };

  if (loading && rows.length === 0) {
    return (
      <div className="flex justify-center py-8 text-zinc-400">
        <Loader2 size={20} className="motion-safe:animate-spin" />
      </div>
    );
  }

  return (
    <div className="space-y-2">
      {error && (
        <p role="alert" className="text-xs text-red-600 dark:text-red-400">
          {error}
        </p>
      )}
      <ul className="divide-y divide-zinc-200 dark:divide-zinc-800 rounded-xl border border-zinc-200 dark:border-zinc-800">
        {rows.map((artist) => (
          <li
            key={artist.id}
            className="flex flex-wrap items-center gap-3 px-4 py-3"
          >
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium text-zinc-900 dark:text-white truncate">
                {artist.name}
              </div>
              <div className="mt-1 flex flex-wrap items-center gap-1.5">
                {artist.fragments.map((fragment, i) => (
                  <span
                    key={`${fragment.name}-${i}`}
                    title={
                      fragment.artist_id != null
                        ? t("library.inventory.phantoms.known")
                        : undefined
                    }
                    className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs ${
                      fragment.artist_id != null
                        ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300"
                        : "bg-zinc-100 text-zinc-700 dark:bg-zinc-800 dark:text-zinc-300"
                    }`}
                  >
                    {fragment.artist_id != null && (
                      <Check size={10} aria-hidden="true" />
                    )}
                    {fragment.name}
                  </span>
                ))}
                <span className="text-xs text-zinc-500 dark:text-zinc-400 tabular-nums">
                  {t("library.inventory.trackCount", {
                    count: artist.track_count,
                  })}
                </span>
              </div>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              <button
                type="button"
                disabled={busy != null || !dismissed.ready}
                onClick={() => dismiss(artist)}
                className="px-3 py-1.5 rounded-lg text-xs text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
              >
                {t("library.inventory.phantoms.dismiss")}
              </button>
              <button
                type="button"
                disabled={busy != null}
                onClick={() => void split(artist)}
                className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 ${
                  armed === artist.id
                    ? "bg-amber-500 text-white hover:bg-amber-600"
                    : "bg-emerald-500 text-white hover:bg-emerald-600"
                }`}
              >
                {busy === artist.id ? (
                  <Loader2
                    size={12}
                    className="motion-safe:animate-spin"
                    aria-hidden="true"
                  />
                ) : (
                  <Split size={12} aria-hidden="true" />
                )}
                {armed === artist.id
                  ? t("library.inventory.phantoms.confirm")
                  : t("library.inventory.phantoms.split")}
              </button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
