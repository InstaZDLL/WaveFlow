import { open } from "@tauri-apps/plugin-dialog";

/**
 * Open the native folder picker and return the absolute path the user
 * selected, or `null` if they cancelled.
 *
 * Thin wrapper around `@tauri-apps/plugin-dialog` so the rest of the app
 * never has to think about its return-type quirks (the plugin returns
 * `string | string[] | null` depending on options).
 */
export async function pickFolder(title?: string): Promise<string | null> {
  const result = await open({
    directory: true,
    multiple: false,
    title,
  });
  if (result == null) return null;
  if (Array.isArray(result)) return result[0] ?? null;
  return result;
}
