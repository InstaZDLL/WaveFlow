import type { TFunction } from "i18next";

import { BackendError } from "./tauri/invoke";

/**
 * What to show the user for an error a command rejected with: the
 * sentence for its kind (`errors.<kind>`) in their language when the
 * backend gave one (`AppError::Shown`). Otherwise `fallback` when the
 * caller has a sentence of its own, else the error's own text — also for
 * a kind this build has no sentence for, so nothing is lost.
 */
export function backendErrorText(
  err: unknown,
  t: TFunction,
  fallback?: string,
): string {
  if (err instanceof BackendError) {
    return t(`errors.${err.kind}`, {
      ...err.params,
      defaultValue: err.message,
    });
  }
  if (fallback !== undefined) return fallback;
  return err instanceof Error ? err.message : String(err);
}
