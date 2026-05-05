import { useCallback, useEffect, useRef, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";

/**
 * State machine for the in-app updater. The hook is silent on
 * platforms where the updater plugin isn't initialized (dev builds
 * deliberately omit it) — `check()` errors are swallowed so the UI
 * stays out of the way.
 */
export type UpdaterState =
  | { kind: "idle" }
  | { kind: "available"; version: string; notes: string | null; update: Update }
  | { kind: "downloading"; progress: number }
  | { kind: "installed" }
  | { kind: "error"; message: string };

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({ kind: "idle" });
  // Holds the Update object across the download in case React's
  // closure stale-state would otherwise drop it.
  const updateRef = useRef<Update | null>(null);

  useEffect(() => {
    let mounted = true;
    (async () => {
      try {
        const update = await check();
        if (!mounted) return;
        if (update?.available) {
          updateRef.current = update;
          setState({
            kind: "available",
            version: update.version,
            notes: update.body ?? null,
            update,
          });
        }
      } catch {
        // Swallow: plugin not registered (dev), no network, or
        // pubkey placeholder still in conf. None of these should
        // surface to the user.
      }
    })();
    return () => {
      mounted = false;
    };
  }, []);

  const install = useCallback(async () => {
    const update = updateRef.current;
    if (!update) return;
    setState({ kind: "downloading", progress: 0 });
    try {
      let downloaded = 0;
      let total = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          const progress = total > 0 ? downloaded / total : 0;
          setState({ kind: "downloading", progress });
        } else if (event.event === "Finished") {
          setState({ kind: "installed" });
        }
      });
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  const dismiss = useCallback(() => {
    setState({ kind: "idle" });
  }, []);

  return { state, install, dismiss };
}
