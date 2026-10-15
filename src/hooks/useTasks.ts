import { useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  listTasks,
  TASKS_CHANGED,
  type TaskSnapshot,
} from "../lib/tauri/tasks";

/**
 * What is running right now, from the backend's task registry
 * (issue #601), kept current by `tasks:changed`.
 *
 * The registry is the source of truth for a long operation. A view that
 * keeps its own "running" flag loses it the moment it unmounts: the
 * settings card used to show Analyse over a sweep still running after a
 * round trip to another page, and its bar stayed full after the end
 * because the view that would have cleared it was gone.
 *
 * **Subscribe before snapshotting.** Tauri does not replay an event to a
 * listener registered a moment too late, so the `listen()` is awaited
 * before `listTasks()` runs; fetching in parallel leaves a window where
 * a task that starts and ends inside it is missed. Subscribing first
 * opens the opposite window — an event can land while `listTasks` is in
 * flight, and the snapshot then describes an older moment — so the
 * snapshot only paints if nothing newer arrived first.
 */
export function useTasks(): TaskSnapshot[] {
  const [tasks, setTasks] = useState<TaskSnapshot[]>([]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    let eventSeen = false;
    (async () => {
      try {
        const off = await listen<TaskSnapshot[]>(TASKS_CHANGED, (event) => {
          eventSeen = true;
          if (!cancelled) setTasks(event.payload);
        });
        // The component can unmount while `listen` is still in flight,
        // and the cleanup below would then have nothing to call — the
        // listener would outlive it for the life of the window.
        if (cancelled) {
          off();
          return;
        }
        unlisten = off;
        const initial = await listTasks();
        if (!cancelled && !eventSeen) setTasks(initial);
      } catch (err) {
        console.warn("[useTasks] subscribe failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return tasks;
}
