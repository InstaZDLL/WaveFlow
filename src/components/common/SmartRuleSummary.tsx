import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Sparkles } from "lucide-react";
import { smartPlaylistKind, type Playlist } from "../../lib/tauri/playlist";
import { getCustomSmartPlaylistRules } from "../../lib/tauri/smart_playlists";
import { listGenres } from "../../lib/tauri/browse";
import { describeRules } from "../../lib/smartRuleSummary";

/**
 * A custom smart playlist's rules, said in words, under its title.
 *
 * Without it the header shows a name and a number, and the one question
 * a smart playlist actually raises — why is *this* track in it — has no
 * answer short of opening the editor. Renders nothing for user
 * playlists and for the built-in families (Daily Mix, On Repeat), whose
 * rules are not a tree and whose names already say what they are.
 */
export function SmartRuleSummary({ playlist }: { playlist: Playlist }) {
  const { t, i18n } = useTranslation();
  /**
   * The sentence, stamped with the playlist it describes.
   *
   * Without the stamp, navigating from one smart playlist to another
   * would show the previous one's rules under the new one's name for as
   * long as the fetch takes — a wrong answer where a blank line is the
   * honest one. Stamping also keeps the "not a smart playlist" case out
   * of the effect body: it is a render-time test, not a state change.
   */
  const [summary, setSummary] = useState<{ id: number; text: string } | null>(
    null,
  );
  const locale = i18n.resolvedLanguage ?? i18n.language;
  const isCustom = smartPlaylistKind(playlist)?.kind === "custom";

  useEffect(() => {
    if (!isCustom) return;
    let alive = true;
    // The rules are fetched rather than parsed out of
    // `playlist.smart_rules`, which is right here in the row: playlists
    // created before the rule tree carry the v1 flat shape, and only
    // the backend deserializer migrates it. Reading the column here
    // would render an empty sentence for exactly the oldest playlists.
    Promise.all([getCustomSmartPlaylistRules(playlist.id), listGenres(null)])
      .then(([rules, genres]) => {
        if (!alive) return;
        setSummary({
          id: playlist.id,
          text: describeRules(rules, { t, locale, genres }),
        });
      })
      .catch(() => {
        // A header line is not worth an error state: a playlist whose
        // rules cannot be read still plays.
        if (alive) setSummary(null);
      });
    return () => {
      alive = false;
    };
    // Keyed on the identity and the rules, not on the playlist object:
    // the view re-fetches the row on every library change, and a new
    // object with the same contents would re-run the whole read for a
    // sentence that cannot have changed.
  }, [playlist.id, playlist.smart_rules, isCustom, t, locale]);

  if (!isCustom || summary?.id !== playlist.id) return null;
  return (
    <p className="flex items-start gap-1.5 text-xs text-zinc-500 dark:text-zinc-400 mb-2">
      <Sparkles size={13} className="mt-0.5 shrink-0 text-violet-500" />
      <span className="line-clamp-2">{summary.text}</span>
    </p>
  );
}
