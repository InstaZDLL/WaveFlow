/**
 * What an album is missing, as `get_album_detail` reports it
 * (`waveflow_core::album_completeness`). Empty lists mean the album is
 * complete — or could not be judged, which the backend never reports as
 * incomplete.
 */
export interface AlbumCompleteness {
  discs: { disc: number; tracks: number[]; at_the_ends: boolean }[];
  missing_discs: number[];
}

/**
 * Numbers as a short list, consecutive runs folded into ranges:
 * `[1, 4, 5, 6, 12]` → `"1, 4–6, 12"`. A missing stretch at the end of a
 * long disc reads as one range rather than a wall of numbers.
 */
export function formatNumberList(numbers: number[]): string {
  const sorted = [...numbers].sort((a, b) => a - b);
  const parts: string[] = [];
  let i = 0;
  while (i < sorted.length) {
    let j = i;
    while (j + 1 < sorted.length && sorted[j + 1] === sorted[j] + 1) j++;
    // Two in a row stay two numbers: "4, 5" reads better than "4–5".
    if (j - i >= 2) {
      parts.push(`${sorted[i]}–${sorted[j]}`);
    } else {
      for (let k = i; k <= j; k++) parts.push(String(sorted[k]));
    }
    i = j + 1;
  }
  return parts.join(", ");
}
