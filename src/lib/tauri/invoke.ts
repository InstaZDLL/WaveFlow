import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";

/**
 * An error the backend wants said in the user's language
 * (`AppError::Shown`): `kind` names the sentence under `errors` in the
 * locales, `params` fill its placeholders, and `message` is the English
 * text the log reads. {@link backendErrorText} turns it into the sentence.
 *
 * Its text is `message`, whether it is read as `.message` or through
 * `String(err)`, so a caller that shows an error as text shows what it
 * always did.
 */
export class BackendError extends Error {
  readonly kind: string;
  readonly params: Record<string, string>;

  constructor(kind: string, message: string, params: Record<string, string>) {
    super(message);
    this.name = "BackendError";
    this.kind = kind;
    this.params = params;
  }

  override toString(): string {
    return this.message;
  }
}

/** Whether a rejection is the `{ kind, message, params }` of `AppError::Shown`. */
function isShown(
  value: unknown,
): value is { kind: string; message: string; params?: unknown } {
  if (typeof value !== "object" || value === null) return false;
  const shown = value as Record<string, unknown>;
  return typeof shown.kind === "string" && typeof shown.message === "string";
}

/** The placeholder values, keeping only the string ones the backend sends. */
function paramsOf(value: unknown): Record<string, string> {
  const params: Record<string, string> = {};
  if (typeof value !== "object" || value === null) return params;
  for (const [name, param] of Object.entries(value)) {
    if (typeof param === "string") params[name] = param;
  }
  return params;
}

/**
 * Tauri's `invoke`, with a rejection that carries a kind turned into a
 * {@link BackendError}. Every other rejection — the bare string most
 * commands reject with — passes through untouched. Every command wrapper
 * imports `invoke` from here, never from `@tauri-apps/api/core` (an ESLint
 * rule holds it).
 */
export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  try {
    return await tauriInvoke<T>(cmd, args, options);
  } catch (err) {
    if (isShown(err)) {
      throw new BackendError(err.kind, err.message, paramsOf(err.params));
    }
    throw err;
  }
}
