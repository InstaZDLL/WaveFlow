import type { CustomRules } from "./tauri/smart_playlists";

/**
 * Ready-made rule sets — somewhere to start, and somewhere to go back
 * to.
 *
 * The blank editor is an empty `all` group, which is a correct starting
 * point and a useless one: it matches the whole library and shows
 * nothing about what a rule can express. These six do, and between them
 * they use most of the vocabulary — a window, a play count, a negation,
 * a bound pair — so picking one and editing it teaches the editor
 * faster than the editor does.
 *
 * Applying one to a playlist that already has rules *replaces* them,
 * which is the more useful half: starting again from something known to
 * work is exactly what repairs a rule set somebody has tangled. Nothing
 * is written until Save, so the replacement is undone by closing the
 * modal — no confirmation step needed to make it safe.
 */
export interface RuleTemplate {
  /** Suffix of `smartPlaylistEditor.templates.*` — the display name. */
  key: string;
  rules: CustomRules;
}

/** Four stars, in the POPM byte the rules store. */
const FOUR_STARS = Math.round((4 / 5) * 255);

export const RULE_TEMPLATES: RuleTemplate[] = [
  {
    key: "recentlyAdded",
    rules: {
      tree: {
        type: "all",
        children: [
          {
            type: "leaf",
            predicate: { kind: "added_in_last_days", value: 30 },
          },
        ],
      },
      sort: "added_desc",
      limit: 100,
    },
  },
  {
    key: "neverPlayed",
    rules: {
      tree: {
        type: "all",
        children: [
          { type: "leaf", predicate: { kind: "play_count_max", value: 0 } },
        ],
      },
      sort: "random",
      limit: 100,
    },
  },
  {
    key: "favourites",
    rules: {
      tree: {
        type: "all",
        children: [
          { type: "leaf", predicate: { kind: "liked" } },
          {
            type: "leaf",
            predicate: { kind: "rating_min", value: FOUR_STARS },
          },
        ],
      },
      sort: "added_desc",
      limit: null,
    },
  },
  {
    key: "forgotten",
    rules: {
      tree: {
        type: "all",
        children: [
          // Played at least once, but not lately: without the play
          // count this would sweep in everything never played at all,
          // which is the *other* template.
          { type: "leaf", predicate: { kind: "play_count_min", value: 1 } },
          {
            type: "not",
            child: {
              type: "leaf",
              predicate: { kind: "played_in_last_days", value: 180 },
            },
          },
        ],
      },
      sort: "random",
      limit: 100,
    },
  },
  {
    key: "hiRes",
    rules: {
      tree: {
        type: "all",
        children: [{ type: "leaf", predicate: { kind: "hi_res" } }],
      },
      sort: "added_desc",
      limit: null,
    },
  },
  {
    key: "upTempo",
    rules: {
      tree: {
        type: "all",
        children: [
          { type: "leaf", predicate: { kind: "bpm_min", value: 120 } },
          { type: "leaf", predicate: { kind: "bpm_max", value: 180 } },
        ],
      },
      sort: "random",
      limit: 50,
    },
  },
];
