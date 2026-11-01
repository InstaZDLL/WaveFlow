import type { FeatureArtKind } from "../components/discovery/FeatureArt";
import type { OpenViewRequest } from "./discovery";

/**
 * The "What's new" panel's pages, written by hand for each version that has
 * something to tell. A correction release adds nothing here, and the panel
 * stays closed for it. The full history stays in About, generated from the
 * commits: this is the handful of things someone would otherwise miss.
 *
 * To add a version: put a release first in `WHATS_NEW_RELEASES`, its
 * `version` being the stable one (a beta of it shows the same pages), and
 * give each page `whatsNew.pages.<id>.{title,body,keywords}` in the 17
 * locales. `keywords` is one string, the words separated by `|`.
 */

/** What "Try it" does. Each kind names something the panel can start
 *  without leaving the user on an empty or misleading screen. */
export type WhatsNewAction =
  | { kind: "view"; request: OpenViewRequest }
  /** The lyrics panel, with the editor open on the playing track. */
  | { kind: "lyricsEditor" }
  /** The immersive view in theater mode, for this visit only. */
  | { kind: "theater" }
  /** The workshop on the album playing, else on the one added last. */
  | { kind: "tagWorkshop" }
  /** The playlist import dialog. */
  | { kind: "playlistImport" };

/**
 * What the action needs before it can show anything. Without it the panel
 * says so and offers the way there instead (`library`: a folder to add;
 * `track`: a library track playing, or the library to pick one from).
 */
export type WhatsNewRequirement = "library" | "track";

export interface WhatsNewPage {
  /** Names its strings: `whatsNew.pages.<id>`. */
  id: string;
  art: FeatureArtKind;
  /** The feature's colour, which the illustration's gradient is mixed from. */
  color: string;
  action: WhatsNewAction;
  requires?: WhatsNewRequirement;
}

export interface WhatsNewRelease {
  /** The stable version these pages belong to, without any pre-release. */
  version: string;
  /** How the panel's title names it. */
  label: string;
  pages: WhatsNewPage[];
}

/** Newest first. */
export const WHATS_NEW_RELEASES: WhatsNewRelease[] = [
  {
    version: "2.0.0",
    label: "2.0",
    pages: [
      {
        id: "fluidInterface",
        art: "fluidInterface",
        color: "#10b981",
        action: {
          kind: "view",
          request: {
            view: "settings",
            settingsCategory: "appearance",
            settingsGroup: "skin",
          },
        },
      },
      {
        id: "automix",
        art: "automix",
        color: "#6366f1",
        action: {
          kind: "view",
          request: {
            view: "settings",
            settingsCategory: "playback",
            settingsGroup: "transitions",
          },
        },
      },
      {
        id: "lyricsEditor",
        art: "lyricsEditor",
        color: "#ec4899",
        action: { kind: "lyricsEditor" },
        requires: "track",
      },
      {
        id: "theater",
        art: "theater",
        color: "#f59e0b",
        action: { kind: "theater" },
        requires: "library",
      },
      {
        id: "tagWorkshop",
        art: "tagWorkshop",
        color: "#0ea5e9",
        action: { kind: "tagWorkshop" },
        requires: "library",
      },
      {
        id: "enrichment",
        art: "enrichment",
        color: "#8b5cf6",
        action: {
          kind: "view",
          request: {
            view: "settings",
            settingsCategory: "library",
            settingsGroup: "enrichment",
          },
        },
      },
      {
        id: "releaseRadar",
        art: "releaseRadar",
        color: "#ef4444",
        action: { kind: "view", request: { view: "release-radar" } },
        requires: "library",
      },
      {
        id: "playlistImport",
        art: "playlistImport",
        color: "#14b8a6",
        action: { kind: "playlistImport" },
      },
    ],
  },
];

interface ParsedVersion {
  core: [number, number, number];
  /** The dot-separated pre-release identifiers, empty for a stable one. */
  pre: string[];
}

const VERSION_PATTERN = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/;

function parseVersion(version: string): ParsedVersion | null {
  const match = VERSION_PATTERN.exec(version.trim());
  if (!match) return null;
  return {
    core: [Number(match[1]), Number(match[2]), Number(match[3])],
    pre: match[4] ? match[4].split(".") : [],
  };
}

function compareCore(a: ParsedVersion, b: ParsedVersion): number {
  for (let i = 0; i < 3; i++) {
    if (a.core[i] !== b.core[i]) return a.core[i] - b.core[i];
  }
  return 0;
}

/** The order of two parsed versions, by the semver rules: a pre-release
 *  comes before its stable, and `beta.10` after `beta.9`. */
function compareParsed(a: ParsedVersion, b: ParsedVersion): number {
  const core = compareCore(a, b);
  if (core !== 0) return core;
  if (a.pre.length === 0 || b.pre.length === 0) {
    return b.pre.length - a.pre.length;
  }
  for (let i = 0; i < Math.min(a.pre.length, b.pre.length); i++) {
    const [x, y] = [a.pre[i], b.pre[i]];
    if (x === y) continue;
    const [xNum, yNum] = [/^\d+$/.test(x), /^\d+$/.test(y)];
    if (xNum && yNum) return Number(x) - Number(y);
    if (xNum !== yNum) return xNum ? -1 : 1;
    return x < y ? -1 : 1;
  }
  return a.pre.length - b.pre.length;
}

/** The newest release at or under `current`: what the panel shows when it
 *  is opened by hand. A beta of a version counts as that version. */
export function latestRelease(current: string): WhatsNewRelease | null {
  const now = parseVersion(current);
  if (!now) return null;
  return (
    WHATS_NEW_RELEASES.find((release) => {
      const version = parseVersion(release.version);
      return version != null && compareCore(version, now) <= 0;
    }) ?? null
  );
}

/**
 * The release someone on `current` has not been shown, given the version
 * the marker holds: the newest one after the marker, up to `current`.
 * `marker` is `null` for an installation that was updated from a version
 * that never wrote one, for which everything counts as new.
 *
 * Only the numbers count, not the pre-release: `2.0.0-beta.1` has shown the
 * pages of 2.0.0, so neither `beta.2` nor the stable 2.0.0 shows them again.
 */
export function pendingRelease(
  marker: string | null,
  current: string,
): WhatsNewRelease | null {
  const release = latestRelease(current);
  if (!release) return null;
  if (marker == null) return release;
  const seen = parseVersion(marker);
  const version = parseVersion(release.version);
  // A marker that cannot be read says nothing about what was seen, and an
  // unasked panel is the lesser fault than one never shown.
  if (!seen) return release;
  return version && compareCore(version, seen) > 0 ? release : null;
}

/** Whether the marker should move to `current`. Never backwards: someone
 *  leaving the beta channel for an older stable has still seen the pages. */
export function markerShouldAdvance(
  marker: string | null,
  current: string,
): boolean {
  const now = parseVersion(current);
  if (!now) return false;
  if (marker == null) return true;
  const seen = parseVersion(marker);
  return seen == null || compareParsed(now, seen) > 0;
}
