import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { Loader2, X, ChevronDown, ChevronUp } from "lucide-react";
import { usePrefersReducedMotion } from "../../hooks/usePrefersReducedMotion";
import {
  listTasks,
  cancelTask,
  TASKS_CHANGED,
  type TaskSnapshot,
} from "../../lib/tauri/tasks";

/**
 * One place that says what is running, and lets the user stop it
 * (issue #601).
 *
 * Before this, a scan reported into the Library view, an analysis sweep
 * into a settings card, and a mirror walk or a backup nowhere at all —
 * so "why is this machine busy" had no answer anywhere in the app.
 *
 * Three decisions worth keeping:
 *
 * - **Subscribe before snapshotting.** Tauri does not replay an event
 *   to a listener registered a moment too late, so the `listen()` is
 *   awaited before `listTasks()` runs. Fetching in parallel leaves a
 *   window where a task that starts and ends inside it is missed
 *   entirely — the same trap the "subscribe first, then snapshot"
 *   invariant was written for.
 * - **A task with no `cancellable` gets no button**, rather than a
 *   disabled one. A backup's only honest stopping point is "after the
 *   current profile", which for most people is the end; a greyed
 *   control there just asks to be clicked.
 * - **Collapsed to one line when several things run.** The bar sits
 *   above the player and must not grow into a panel — three
 *   simultaneous tasks are unusual but a scan plus its auto-analysis is
 *   not.
 */
export function TaskStatusBar() {
  const { t } = useTranslation();
  const [tasks, setTasks] = useState<TaskSnapshot[]>([]);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | null = null;
    // Subscribing first closes one window and opens another: an event
    // can now land *while* `listTasks` is in flight, and the snapshot
    // that comes back describes an older moment. Applying it would
    // resurrect a task that has just finished, with a cancel button
    // that does nothing. So the snapshot only paints if nothing newer
    // arrived first.
    let eventSeen = false;
    (async () => {
      try {
        // Order matters — see the component docs.
        unlisten = await listen<TaskSnapshot[]>(TASKS_CHANGED, (event) => {
          eventSeen = true;
          if (!cancelled) setTasks(event.payload);
        });
        const initial = await listTasks();
        if (!cancelled && !eventSeen) setTasks(initial);
      } catch (err) {
        console.warn("[TaskStatusBar] subscribe failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const stop = useCallback((id: number) => {
    void cancelTask(id).catch((err) =>
      console.warn("[TaskStatusBar] cancel failed", err),
    );
  }, []);

  if (tasks.length === 0) return null;

  const shown = expanded ? tasks : tasks.slice(0, 1);
  const hidden = tasks.length - shown.length;

  return (
    // Not a live region. The counters inside change several times a
    // second, and `aria-live` on a container that holds them makes a
    // screen reader read the whole bar out over and over — which is
    // worse than silence for the user it was meant to help. The
    // progress semantics live on each row's `role="progressbar"`, which
    // assistive technology reports on demand.
    <section
      aria-label={t("tasks.barLabel")}
      className="shrink-0 border-t border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-900 px-4 py-2 space-y-2"
    >
      {shown.map((task) => (
        <TaskRow key={task.id} task={task} onStop={stop} />
      ))}
      {tasks.length > 1 && (
        <button
          type="button"
          onClick={() => setExpanded((value) => !value)}
          className="flex items-center gap-1 text-[11px] text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-100 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded"
          aria-expanded={expanded}
        >
          {expanded ? <ChevronDown size={12} /> : <ChevronUp size={12} />}
          <span>
            {expanded
              ? t("tasks.collapse")
              : t("tasks.more", { count: hidden })}
          </span>
        </button>
      )}
    </section>
  );
}

function TaskRow({
  task,
  onStop,
}: {
  task: TaskSnapshot;
  onStop: (id: number) => void;
}) {
  const { t } = useTranslation();
  // Both indicators here are ambient, continuous motion — the kind
  // `prefers-reduced-motion` exists for. The counter and the bar still
  // say everything the animation was decorating.
  const reducedMotion = usePrefersReducedMotion();
  // `0` is the wire's way of saying "no idea how much there is" — a
  // mirror walk does not know its page count until the server answers.
  const determinate = task.total > 0;
  const percent = determinate
    ? Math.min(100, Math.round((task.current / task.total) * 100))
    : 0;
  // `defaultValue` rather than a bare key: a task kind added on the
  // backend without its 17 locale entries should read as itself, not as
  // a raw `tasks.kinds.foo` path in the middle of the interface.
  const label = t(`tasks.kinds.${task.kind}`, { defaultValue: task.kind });

  return (
    <div className="flex items-center gap-3">
      <Loader2
        size={14}
        className={`shrink-0 text-emerald-500 ${reducedMotion ? "" : "animate-spin"}`}
        aria-hidden="true"
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="text-xs font-medium text-zinc-800 dark:text-zinc-100 shrink-0">
            {label}
          </span>
          {task.detail && (
            <span
              className="text-[11px] text-zinc-500 dark:text-zinc-400 truncate"
              title={task.detail}
            >
              {task.detail}
            </span>
          )}
          {determinate && (
            <span className="text-[11px] text-zinc-500 dark:text-zinc-400 ml-auto shrink-0 tabular-nums">
              {task.current} / {task.total}
            </span>
          )}
        </div>
        <div
          className="mt-1 h-1 rounded-full bg-zinc-200 dark:bg-zinc-800 overflow-hidden"
          role="progressbar"
          aria-valuenow={determinate ? percent : undefined}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-label={label}
        >
          <div
            className={
              determinate
                ? "h-full bg-emerald-500 transition-[width] duration-300"
                : `h-full w-1/3 bg-emerald-500 ${
                    reducedMotion ? "opacity-70" : "animate-pulse"
                  }`
            }
            style={determinate ? { width: `${percent}%` } : undefined}
          />
        </div>
      </div>
      {task.cancellable && (
        <button
          type="button"
          onClick={() => onStop(task.id)}
          disabled={task.cancelling}
          aria-label={t("tasks.cancel", { task: label })}
          title={
            task.cancelling ? t("tasks.stopping") : t("tasks.cancel", { task: label })
          }
          className="shrink-0 p-1.5 rounded-lg text-zinc-500 hover:text-zinc-900 hover:bg-zinc-200 disabled:opacity-40 disabled:hover:bg-transparent dark:text-zinc-400 dark:hover:text-zinc-100 dark:hover:bg-zinc-800 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
        >
          <X size={14} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}
