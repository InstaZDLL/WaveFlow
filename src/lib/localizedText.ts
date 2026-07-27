/**
 * Plugin-authored strings that may carry per-language variants.
 *
 * Plugin names, descriptions and option labels live in each plugin's
 * `manifest.toml` (and store descriptions in the registry's
 * `registry.json`) — outside the app's i18next resource files, so
 * they can't be translated by the usual `t()` path. Instead the
 * format itself accepts a language map, the backend hands the whole
 * value through untouched, and the UI resolves it here against the
 * active language. Switching languages therefore re-renders these
 * strings instantly, with no backend round-trip.
 *
 * This is the mirror of `LocalizedString` in
 * `src-tauri/crates/core/src/plugin/manifest.rs` — the two fallback
 * chains MUST agree, so change them together.
 */
export type LocalizedText = string | Record<string, string>;

/** A translation slot only counts if it holds something to render. */
function usable(text: string | null | undefined): string | null {
  return text != null && text.trim() !== "" ? text : null;
}

/**
 * Best text for `lang`, following the format's documented chain:
 *
 * 1. exact match (`pt-BR`),
 * 2. the base language (`pt-BR` → `pt`),
 * 3. `en` — the format's documented default,
 * 4. any entry, lowest key first, so a manifest shipping only e.g.
 *    `de` still renders something instead of a blank row.
 *
 * **Blank entries are skipped at every step**, not treated as a hit —
 * `{ fr: "", en: "…" }` renders the English rather than nothing. An
 * untranslated slot left empty is a common authoring accident, and
 * letting it win would blank a store card or, worse, leave an option
 * control with no accessible name (callers fall back to the option
 * key on `null`, which an empty string would defeat).
 *
 * Returns `null` for a nullish input, an empty map, or a map with
 * nothing renderable in it. The manifest validator refuses an empty
 * map at parse time, but a registry entry comes off the network, so
 * callers must handle it.
 *
 * The `typeof` guard is what keeps every pre-existing plugin working:
 * a plain `description = "…"` arrives as a string and short-circuits
 * to itself for every language.
 */
export function resolveLocalizedText(
  value: LocalizedText | null | undefined,
  lang: string,
): string | null {
  if (value == null) return null;
  if (typeof value === "string") return usable(value);

  const base = lang.split("-")[0];
  const hit = usable(value[lang]) ?? usable(value[base]) ?? usable(value.en);
  if (hit != null) return hit;

  // Last resort: sort so the pick is deterministic and matches the
  // Rust side's BTreeMap ordering rather than JSON key order.
  for (const key of Object.keys(value).sort()) {
    const candidate = usable(value[key]);
    if (candidate != null) return candidate;
  }
  return null;
}
