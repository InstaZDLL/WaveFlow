/**
 * Human-readable byte sizes.
 *
 * A folder can hold anything from one MP3 to a hundred gigabytes of
 * rips, so the unit has to scale — the two ad-hoc formatters this
 * repository already had stop at MB, which reads as "184000.0 MB" on a
 * library root (`DuplicatesModal`, `TrackPropertiesModal`). Neither is
 * touched here: their strings are user-visible and one of them is
 * localised ("Mo"), so folding them in belongs to a pass that owns
 * those views.
 *
 * Units are the SI-style symbols rather than i18n keys: they are the
 * same in every locale WaveFlow ships, and a key per unit would be four
 * keys × 17 files that always hold the same value.
 */
export function formatBytes(bytes: number, locale?: string): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  // The number is formatted for the language the UI is in, not the one
  // the browser reports: WaveFlow ships 17 locales and the user picks
  // one, and half of them write "9,4 GB" rather than "9.4 GB".
  const format = (value: number, digits: number) =>
    new Intl.NumberFormat(locale, {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    }).format(value);
  if (bytes < 1024) return `${format(bytes, 0)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // One decimal below 100, none above: "9.4 GB" is worth the digit,
  // "184.0 GB" is not.
  return `${format(value, value < 100 ? 1 : 0)} ${units[unit]}`;
}
