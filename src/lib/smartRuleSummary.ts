import type { TFunction } from "i18next";
import type {
  CustomRules,
  CustomSort,
  Predicate,
  RuleNode,
} from "./tauri/smart_playlists";
import type { GenreRow } from "./tauri/browse";

/**
 * A smart playlist said in words, for the playlist header.
 *
 * A rule tree explains itself badly once it is stored: the header shows
 * a name and a track count, and nothing about *why* those tracks are
 * the ones in it. Opening the editor answers that, but only for
 * somebody who already knows the editor exists — and the question
 * ("why is this track here?") is usually asked by somebody who does
 * not.
 *
 * The sentence is built from translated fragments rather than a
 * generated clause per language: every predicate is one key with its
 * value interpolated, and the groups are joined by a separator. That
 * keeps a translator's job to short phrases, and keeps the recursion
 * out of the locale files.
 *
 * Deliberately not exhaustive prose: a deeply nested tree collapses to
 * parenthesised groups, which is readable for the two or three levels
 * people actually build and honest — rather than wrong — beyond that.
 */

/** Everything the summary needs beyond the rules themselves. */
export interface SummaryContext {
  t: TFunction;
  /** BCP-47 tag for number formatting — `i18n.resolvedLanguage`, the
   *  same source every other localized number in the app uses. */
  locale: string;
  /** Genre names by id; a genre the library no longer has falls back
   *  to its id so the sentence stays truthful about the rule. */
  genres: GenreRow[];
}

/** Longest sentence we build before giving up on detail. */
const MAX_CHARS = 240;

export function describeRules(rules: CustomRules, ctx: SummaryContext): string {
  const conditions = describeNode(rules.tree, ctx, true);
  const parts = [conditions];

  // The sort only earns its place in the sentence when a limit makes it
  // decide *membership*. Without one it decides the order of a list the
  // user is looking at, which the list already shows.
  if (rules.limit != null) {
    parts.push(
      ctx.t("smartRuleSummary.limit", {
        value: formatNumber(rules.limit, ctx),
      }),
    );
    parts.push(ctx.t(`smartRuleSummary.sort.${sortKey(rules.sort)}`));
  }

  const sentence = parts.filter(Boolean).join(" · ");
  return sentence.length > MAX_CHARS
    ? `${sentence.slice(0, MAX_CHARS - 1).trimEnd()}…`
    : sentence;
}

/**
 * One node as a phrase.
 *
 * `top` suppresses the parentheses around the outermost group: the
 * whole sentence is that group, and wrapping it adds a pair of brackets
 * around everything for no gain.
 */
export function describeNode(
  node: RuleNode,
  ctx: SummaryContext,
  top = false,
): string {
  switch (node.type) {
    case "all":
    case "any": {
      if (node.children.length === 0) {
        // An empty `all` is the canonical "no filter" root and matches
        // every available track; an empty `any` matches nothing. Both
        // are reachable from the editor, and the second one is worth
        // saying out loud — it is the shape of a playlist that comes
        // out empty for a reason nothing else shows.
        return ctx.t(
          node.type === "all"
            ? "smartRuleSummary.everything"
            : "smartRuleSummary.nothing",
        );
      }
      const joiner = ctx.t(
        node.type === "all" ? "smartRuleSummary.and" : "smartRuleSummary.or",
      );
      const inner = node.children
        .map((child) => describeNode(child, ctx))
        .join(joiner);
      return top || node.children.length === 1 ? inner : `(${inner})`;
    }
    case "not":
      return ctx.t("smartRuleSummary.not", {
        inner: describeNode(node.child, ctx),
      });
    case "leaf":
      return describePredicate(node.predicate, ctx);
  }
}

function describePredicate(pred: Predicate, ctx: SummaryContext): string {
  const { t } = ctx;
  const p = (key: string, vars?: Record<string, unknown>) =>
    t(`smartRuleSummary.predicates.${key}`, vars ?? {});

  switch (pred.kind) {
    case "title_contains":
      return p("titleContains", { value: pred.value });
    case "artist_contains":
      return p("artistContains", { value: pred.value });
    case "album_contains":
      return p("albumContains", { value: pred.value });
    case "path_contains":
      return p("pathContains", { value: pred.value });
    case "genre_is":
      return p("genreIs", { value: genreName(pred.value, ctx) });
    case "year_min":
      return p("yearMin", { value: pred.value });
    case "year_max":
      return p("yearMax", { value: pred.value });
    case "disc_number_is":
      return p("discNumberIs", { value: pred.value });
    case "liked":
      return p("liked");
    case "rating_min":
      // Stars rather than a number with a counted noun: the glyphs say
      // the same thing in every language and need no plural form.
      return p("ratingMin", { value: stars(pred.value) });
    case "format":
      return p("format", { value: pred.value.toUpperCase() });
    case "hi_res":
      return p("hiRes");
    case "sample_rate_min":
      return p("sampleRateMin", { value: kilohertz(pred.value, ctx) });
    case "bit_depth_min":
      return p("bitDepthMin", { value: pred.value });
    case "bpm_min":
      return p("bpmMin", { value: formatNumber(pred.value, ctx) });
    case "bpm_max":
      return p("bpmMax", { value: formatNumber(pred.value, ctx) });
    case "duration_min_ms":
      return p("durationMinMs", { value: minutes(pred.value, ctx) });
    case "duration_max_ms":
      return p("durationMaxMs", { value: minutes(pred.value, ctx) });
    case "play_count_min":
      return p("playCountMin", { count: pred.value });
    case "play_count_max":
      // Zero is not "at most nothing", it is *never played* — the whole
      // reason the predicate exists, and a sentence that says it any
      // other way reads as a bug.
      return pred.value === 0
        ? p("neverPlayed")
        : p("playCountMax", { count: pred.value });
    case "played_in_last_days":
      return p("playedInLastDays", { count: pred.value });
    case "added_in_last_days":
      return p("addedInLastDays", { count: pred.value });
    case "tag_present":
      return p("tagPresent", { key: pred.key });
    case "tag_contains":
      return p("tagContains", { key: pred.key, value: pred.value });
  }
}

function genreName(id: number, ctx: SummaryContext): string {
  return ctx.genres.find((g) => g.id === id)?.name ?? String(id);
}

function stars(popm: number): string {
  const n = Math.max(1, Math.min(5, Math.round((popm / 255) * 5)));
  return "★".repeat(n);
}

function minutes(ms: number, ctx: SummaryContext): string {
  return ctx.t("smartRuleSummary.minutes", {
    value: formatNumber(Math.round(ms / 60_000), ctx),
  });
}

function formatNumber(value: number, ctx: SummaryContext): string {
  return new Intl.NumberFormat(ctx.locale).format(value);
}

/**
 * A sample rate in kHz, with the reader's decimal separator.
 *
 * `toFixed` always writes a dot, so 88.2 kHz reached a French or German
 * reader as "88.2" in a sentence where every other number is written
 * with a comma.
 */
function kilohertz(hz: number, ctx: SummaryContext): string {
  return new Intl.NumberFormat(ctx.locale, {
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
  }).format(hz / 1000);
}

function sortKey(sort: CustomSort | null | undefined): string {
  switch (sort ?? "added_desc") {
    case "added_asc":
      return "addedAsc";
    case "year_desc":
      return "yearDesc";
    case "year_asc":
      return "yearAsc";
    case "title_asc":
      return "titleAsc";
    case "artist_asc":
      return "artistAsc";
    case "random":
      return "random";
    default:
      return "addedDesc";
  }
}
