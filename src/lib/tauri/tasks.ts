import { invoke } from "@tauri-apps/api/core";

/** Event carrying the whole list whenever it changes. */
export const TASKS_CHANGED = "tasks:changed";

/**
 * One running long operation (issue #601).
 *
 * Mirrors `tasks::TaskSnapshot`. `kind` is a stable slug the UI turns
 * into localized copy — treat it like an event name, not a label.
 */
export interface TaskSnapshot {
  id: number;
  kind: string;
  /** A path or a title. Deliberately not localized. */
  detail: string | null;
  current: number;
  /** `0` means indeterminate. */
  total: number;
  cancellable: boolean;
  /** A stop has been asked for and the task has not exited yet. */
  cancelling: boolean;
}

export function listTasks(): Promise<TaskSnapshot[]> {
  return invoke<TaskSnapshot[]>("list_tasks");
}

/**
 * Ask a task to stop, through whatever mechanism it registered.
 *
 * `false` means there was nothing to ask — it finished between the
 * render and the click, it is already stopping, or it never offered a
 * way out. A cancel button races the task it cancels by construction,
 * so none of those is an error worth showing.
 */
export function cancelTask(id: number): Promise<boolean> {
  return invoke<boolean>("cancel_task", { id });
}
