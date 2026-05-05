import { open, save } from "@tauri-apps/plugin-dialog";

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

/**
 * Open the native file picker filtered to a list of extensions
 * (without the leading dot, e.g. `["lrc", "txt"]`). Returns the
 * absolute path or `null` when the user cancelled.
 */
export async function pickFile(
  extensions: string[],
  title?: string,
): Promise<string | null> {
  const result = await open({
    directory: false,
    multiple: false,
    title,
    filters:
      extensions.length > 0
        ? [{ name: extensions.join(", ").toUpperCase(), extensions }]
        : undefined,
  });
  if (result == null) return null;
  if (Array.isArray(result)) return result[0] ?? null;
  return result;
}

/**
 * Open the native save dialog. Returns the absolute path the user
 * picked, or `null` when they cancelled. `defaultName` pre-fills the
 * suggested filename (with extension), `extensions` constrains the
 * picker to specific suffixes.
 */
export async function pickSaveFile(
  defaultName: string,
  extensions: string[],
  title?: string,
): Promise<string | null> {
  const result = await save({
    title,
    defaultPath: defaultName,
    filters:
      extensions.length > 0
        ? [{ name: extensions.join(", ").toUpperCase(), extensions }]
        : undefined,
  });
  return result ?? null;
}
