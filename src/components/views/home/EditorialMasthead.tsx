import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

/**
 * Real broadsheet masthead for the Editorial skin: edition info
 * row, big Didone title, italic subtitle quote between two
 * rules. Mounted by HomeView only when the active skin is
 * Editorial — other skins never pay for it.
 *
 * The date renders in the user's active i18n locale via
 * `Intl.DateTimeFormat`, so users get "Jeudi 18 juin 2026" /
 * "Thursday June 18 2026" / "木曜日 2026年6月18日" without any
 * translation table to maintain. We snapshot the date once at
 * mount + refresh once at local midnight — the app doesn't
 * care about the second granularity here, and reading
 * `Date.now()` in render would defeat React's memoisation.
 */
export function EditorialMasthead() {
  const { t, i18n } = useTranslation();
  const [today, setToday] = useState(() => new Date());

  useEffect(() => {
    const now = new Date();
    const msToMidnight =
      new Date(
        now.getFullYear(),
        now.getMonth(),
        now.getDate() + 1,
        0,
        0,
        0,
      ).getTime() - now.getTime();
    const id = window.setTimeout(() => setToday(new Date()), msToMidnight);
    return () => window.clearTimeout(id);
    // `today` is in the dep list on purpose — when the timer
    // fires and `setToday` swaps the state, we want this effect
    // to re-run so the NEXT midnight gets scheduled. This is the
    // chain that keeps the masthead date current across day
    // boundaries without a wall-clock interval.
  }, [today]);

  const dateLabel = new Intl.DateTimeFormat(i18n.language, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
  }).format(today);

  return (
    <div className="editorial-masthead">
      <div className="editorial-masthead__strip">
        <span>{t("editorial.masthead.edition", "Édition du Matin")}</span>
        <span>{dateLabel}</span>
        <span>{t("editorial.masthead.price", "Prix : Inclus")}</span>
      </div>
      <h1 className="editorial-masthead__title">
        WaveFlow
        <span className="editorial-masthead__title-suffix">Gazette</span>
      </h1>
      <hr className="editorial-masthead__rule" />
      <p className="editorial-masthead__subtitle">
        {t(
          "editorial.masthead.subtitle",
          '"Toute la musique qui mérite d\'être imprimée" — Votre sélection quotidienne, sur mesure.',
        )}
      </p>
    </div>
  );
}
