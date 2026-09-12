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
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // One decimal below 100, none above: "9.4 GB" is worth the digit,
  // "184.0 GB" is not.
  return `${value < 100 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}
