import { useCallback, useEffect, useState } from "react";
import {
  installPluginFromRegistry,
  listPluginMarketplace,
  type MarketplaceEntry,
} from "../../lib/tauri/plugins";
import { RELEASE_RADAR_PLUGIN_ID } from "../../lib/tauri/releaseRadar";
import { PLUGIN_AVAILABILITY_EVENT } from "../../hooks/usePluginAvailability";
import type { FeatureArtKind } from "../discovery/FeatureArt";

/**
 * What a plugin is for, read from the registry's own tags. The order is
 * the order the journey shows them in; a plugin tagged for two usages
 * lands under the first.
 */
export const PLUGIN_USAGES = ["lyrics", "canvas", "artwork", "radio"] as const;
export type PluginUsage = (typeof PLUGIN_USAGES)[number];

export const USAGE_ART: Record<PluginUsage, FeatureArtKind> = {
  lyrics: "lyricsSources",
  canvas: "canvas",
  artwork: "motionArtwork",
  radio: "plugins",
};

/** The registry tag that puts an entry under a usage. */
const USAGE_TAG: Record<PluginUsage, string> = {
  lyrics: "lyrics",
  canvas: "canvas",
  artwork: "artwork",
  radio: "radio",
};

/** Lyrics synced word by word: the registry tags them so. */
export const WORD_SYNCED_TAG = "word-synced";

export function usageOf(entry: MarketplaceEntry): PluginUsage | null {
  return (
    PLUGIN_USAGES.find((usage) => entry.tags.includes(USAGE_TAG[usage])) ?? null
  );
}

/**
 * The recommendation: first-party plugins this build can run, under a
 * usage the journey knows how to tell. The retired Release Radar plugin
 * stays out — the native view replaced it.
 */
function recommended(entry: MarketplaceEntry): boolean {
  return (
    entry.official &&
    entry.compatible &&
    entry.id !== RELEASE_RADAR_PLUGIN_ID &&
    usageOf(entry) != null
  );
}

export type InstallState =
  { kind: "installing" } | { kind: "failed"; message: string };

export type PluginCatalogue =
  | { status: "loading" }
  /** Offline, or no registry answered: the journey presents the usages
   *  and points at Extensions, never an error. */
  | { status: "unavailable" }
  | { status: "ready"; entries: MarketplaceEntry[] };

/**
 * The store's recommended entries, loaded once for the journey, and their
 * installs. An install goes through `installPluginFromRegistry`, the same
 * path as the Extensions page, and announces itself the same way.
 *
 * Nothing is fetched until `wanted` first holds — the screens that show the
 * store — so a journey closed before them never reaches the registry.
 */
export function usePluginCatalogue(wanted: boolean) {
  const [catalogue, setCatalogue] = useState<PluginCatalogue>({
    status: "loading",
  });
  const [installs, setInstalls] = useState<Record<string, InstallState>>({});

  // Latched: once asked for, the catalogue stays for the whole journey.
  const [requested, setRequested] = useState(false);
  if (wanted && !requested) setRequested(true);

  useEffect(() => {
    if (!requested) return;
    let cancelled = false;
    listPluginMarketplace()
      .then((entries) => {
        if (cancelled) return;
        setCatalogue({ status: "ready", entries: entries.filter(recommended) });
      })
      .catch((err) => {
        console.warn("[Onboarding] plugin store unavailable", err);
        if (!cancelled) setCatalogue({ status: "unavailable" });
      });
    return () => {
      cancelled = true;
    };
  }, [requested]);

  const install = useCallback(async (id: string) => {
    setInstalls((cur) => ({ ...cur, [id]: { kind: "installing" } }));
    try {
      await installPluginFromRegistry(id);
      window.dispatchEvent(new CustomEvent(PLUGIN_AVAILABILITY_EVENT));
      setCatalogue((cur) =>
        cur.status === "ready"
          ? {
              status: "ready",
              entries: cur.entries.map((e) =>
                e.id === id ? { ...e, installed: true } : e,
              ),
            }
          : cur,
      );
      setInstalls((cur) => {
        const next = { ...cur };
        delete next[id];
        return next;
      });
    } catch (err) {
      console.error("[Onboarding] install failed", err);
      setInstalls((cur) => ({
        ...cur,
        [id]: { kind: "failed", message: String(err) },
      }));
    }
  }, []);

  return { catalogue, installs, install };
}
