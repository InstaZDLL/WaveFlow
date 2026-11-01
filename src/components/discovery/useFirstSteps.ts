import { useCallback, useEffect, useState } from "react";
import { useProfile } from "../../hooks/useProfile";
import {
  DISCOVERIES,
  DISCOVERY_CHANGED_EVENT,
  DISCOVERY_HIDDEN_KEY,
  DISCOVERY_SEEDED_KEY,
  discoveryKey,
  type DiscoveryId,
} from "../../lib/discovery";
import { getProfileSetting, setProfileSetting } from "../../lib/tauri/profile";
import { listPlaylists, smartPlaylistKind } from "../../lib/tauri/playlist";
import { listLibraries } from "../../lib/tauri/library";
import { listInstalledPlugins } from "../../lib/tauri/plugins";
import { getEnrichmentStatus } from "../../lib/tauri/enrichment";
import { getDesktopLyricsStatus } from "../../lib/tauri/desktopLyrics";

interface FirstStepsState {
  profileId: number;
  done: ReadonlySet<DiscoveryId>;
  hidden: boolean;
}

/**
 * What the profile's data already shows it has used: a smart playlist of
 * its own, a plugin from the store, an enrichment pass, the desktop
 * lyrics open right now. The rest leaves no trace to read and stays
 * unticked until it is used.
 *
 * Only for a profile that already holds a library: one that existed
 * before the card. A new profile reaches Home with an empty one and must
 * start with nothing ticked, since plugins and the desktop lyrics window
 * belong to the app, not to the profile, and would tick it for a use made
 * in another.
 */
async function alreadyUsed(): Promise<DiscoveryId[]> {
  const libraries = await listLibraries();
  if (!libraries.some((l) => l.folder_count > 0)) return [];
  const used: DiscoveryId[] = [];
  const checks: Array<Promise<void>> = [
    listPlaylists().then((playlists) => {
      if (playlists.some((p) => smartPlaylistKind(p)?.kind === "custom")) {
        used.push("smartPlaylist");
      }
    }),
    listInstalledPlugins().then((plugins) => {
      if (plugins.some((p) => !p.bundled)) used.push("plugin");
    }),
    getEnrichmentStatus().then(({ stats }) => {
      const decided =
        stats.albums_applied +
        stats.albums_review +
        stats.albums_rejected +
        stats.albums_no_match +
        stats.artists_applied +
        stats.artists_review +
        stats.artists_rejected +
        stats.artists_no_match;
      if (decided > 0) used.push("enrichment");
    }),
    getDesktopLyricsStatus().then((status) => {
      if (status.open) used.push("desktopLyrics");
    }),
  ];
  // One unreadable source leaves its item unticked, not the others.
  const results = await Promise.allSettled(checks);
  for (const r of results) {
    if (r.status === "rejected") {
      console.warn("[FirstSteps] could not read a prior use", r.reason);
    }
  }
  return used;
}

/**
 * The Home's first steps for the active profile: which features are
 * ticked, whether the card was closed. Read with the profile's id, so a
 * switch never paints the previous profile's ticks; re-read when a tick
 * is written anywhere (`DISCOVERY_CHANGED_EVENT`).
 *
 * The first read on a profile that was never seeded ticks what its data
 * already shows — the profiles that existed before the card — and
 * records that it did, so an item unticked later is not ticked again.
 */
export function useFirstSteps() {
  const profileId = useProfile().activeProfile?.id ?? null;
  const [state, setState] = useState<FirstStepsState | null>(null);
  const [revision, setRevision] = useState(0);

  useEffect(() => {
    const bump = () => setRevision((r) => r + 1);
    window.addEventListener(DISCOVERY_CHANGED_EVENT, bump);
    return () => window.removeEventListener(DISCOVERY_CHANGED_EVENT, bump);
  }, []);

  useEffect(() => {
    if (profileId == null) return;
    let cancelled = false;
    void (async () => {
      try {
        const read = (key: string) => getProfileSetting(key, profileId);
        if ((await read(DISCOVERY_SEEDED_KEY)) !== "true") {
          const used = await alreadyUsed();
          await Promise.all(
            used.map((id) =>
              setProfileSetting(discoveryKey(id), "true", "bool", profileId),
            ),
          );
          await setProfileSetting(
            DISCOVERY_SEEDED_KEY,
            "true",
            "bool",
            profileId,
          );
        }
        const [hidden, ...flags] = await Promise.all([
          read(DISCOVERY_HIDDEN_KEY),
          ...DISCOVERIES.map((id) => read(discoveryKey(id))),
        ]);
        if (cancelled) return;
        setState({
          profileId,
          hidden: hidden === "true",
          done: new Set(DISCOVERIES.filter((_, i) => flags[i] === "true")),
        });
      } catch (err) {
        // A refused read (the profile changed under it) or a failed one:
        // nothing is drawn rather than another profile's ticks.
        console.error("[FirstSteps] read failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [profileId, revision]);

  const hide = useCallback(() => {
    if (profileId == null) return;
    setState((cur) =>
      cur && cur.profileId === profileId ? { ...cur, hidden: true } : cur,
    );
    setProfileSetting(DISCOVERY_HIDDEN_KEY, "true", "bool", profileId).catch(
      (err) => console.error("[FirstSteps] hide failed", err),
    );
  }, [profileId]);

  const current = state && state.profileId === profileId ? state : null;
  return {
    ready: current != null,
    done: current?.done ?? new Set<DiscoveryId>(),
    hidden: current?.hidden ?? true,
    hide,
  };
}
