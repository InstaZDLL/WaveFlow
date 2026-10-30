import { useCallback, useRef, useState } from "react";

/** How many steps are kept. */
const LIMIT = 200;
/** Edits with the same key this close together undo as one. */
const COALESCE_MS = 800;

/**
 * State with undo and redo, for an editor holding one document.
 *
 * `set` takes an updater and an optional key: consecutive edits that
 * share it — typing in one field — collapse into a single step, so an
 * undo takes back the word typed rather than its last letter. An updater
 * that returns the state it was given records nothing.
 */
export function useUndoHistory<T>(initial: T) {
  const [state, setState] = useState(initial);
  const [depth, setDepth] = useState({ past: 0, future: 0 });
  const past = useRef<T[]>([]);
  const future = useRef<T[]>([]);
  const last = useRef<{ key: string; at: number } | null>(null);
  // Read by `set` synchronously: React state lags a render behind, and
  // two edits in one event would otherwise both start from the same one.
  const current = useRef(initial);

  const commit = useCallback((next: T) => {
    current.current = next;
    setState(next);
    setDepth({ past: past.current.length, future: future.current.length });
  }, []);

  const set = useCallback(
    (update: (prev: T) => T, key?: string) => {
      const prev = current.current;
      const next = update(prev);
      if (next === prev) return;
      const now = Date.now();
      const coalesce =
        key !== undefined &&
        last.current?.key === key &&
        now - last.current.at < COALESCE_MS;
      if (!coalesce) {
        past.current.push(prev);
        if (past.current.length > LIMIT) past.current.shift();
      }
      future.current = [];
      last.current = key === undefined ? null : { key, at: now };
      commit(next);
    },
    [commit],
  );

  const undo = useCallback(() => {
    const prev = past.current.pop();
    if (prev === undefined) return;
    future.current.push(current.current);
    last.current = null;
    commit(prev);
  }, [commit]);

  const redo = useCallback(() => {
    const next = future.current.pop();
    if (next === undefined) return;
    past.current.push(current.current);
    last.current = null;
    commit(next);
  }, [commit]);

  /** Start over from `value`, with nothing to undo. */
  const reset = useCallback(
    (value: T) => {
      past.current = [];
      future.current = [];
      last.current = null;
      commit(value);
    },
    [commit],
  );

  return {
    state,
    set,
    undo,
    redo,
    reset,
    canUndo: depth.past > 0,
    canRedo: depth.future > 0,
  };
}
