import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  checkForUpdate,
  installUpdate,
  UPDATER_PROGRESS_EVENT,
  UPDATER_RECHECK_EVENT,
  type UpdateProgress,
} from "../lib/tauri/updater";

/**
 * State machine for the in-app updater. The check + download run in
 * Rust (so the active release channel — stable / beta — can pick the
 * endpoint at runtime, which the JS `check()` can't do). The hook is
 * silent on builds without the updater (dev, app-store): the backend
 * returns `null` and `install_update` errors are surfaced only if the
 * user explicitly triggered an install.
 */
export type UpdaterState =
  | { kind: "idle" }
  | { kind: "available"; version: string; notes: string | null }
  | { kind: "downloading"; progress: number }
  | { kind: "installed" }
  | { kind: "error"; message: string };

export function useUpdater() {
  const [state, setState] = useState<UpdaterState>({ kind: "idle" });
  // Guards against a stale check (e.g. after a channel switch) clobbering
  // a newer one — only the latest check id may write state.
  const checkSeq = useRef(0);

  const runCheck = useCallback(async () => {
    const seq = ++checkSeq.current;
    try {
      const update = await checkForUpdate();
      if (seq !== checkSeq.current) return;
      if (update) {
        setState({
          kind: "available",
          version: update.version,
          notes: update.notes,
        });
      } else {
        setState({ kind: "idle" });
      }
    } catch {
      // Swallow: updater unavailable (dev), no network, or pubkey
      // placeholder. None of these should surface on a passive check.
      if (seq === checkSeq.current) setState({ kind: "idle" });
    }
  }, []);

  useEffect(() => {
    // runCheck awaits an IPC round-trip before any setState, so the
    // state write lands in a later microtask — not the synchronous
    // cascading render the rule guards against.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void runCheck();
  }, [runCheck]);

  // Re-check when the user flips the beta channel in Settings so the
  // banner reflects the new endpoint without a relaunch.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const handler = () => void runCheck();
    window.addEventListener(UPDATER_RECHECK_EVENT, handler);
    return () => window.removeEventListener(UPDATER_RECHECK_EVENT, handler);
  }, [runCheck]);

  // Bridge backend download progress into the downloading state.
  useEffect(() => {
    const unlisten = listen<UpdateProgress>(UPDATER_PROGRESS_EVENT, (event) => {
      const { downloaded, total } = event.payload;
      const progress = total > 0 ? downloaded / total : 0;
      setState({ kind: "downloading", progress });
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  const install = useCallback(async () => {
    setState({ kind: "downloading", progress: 0 });
    try {
      await installUpdate();
      // On Windows the installer launches and the app exits before this
      // resolves; reaching here means a clean hand-off on other targets.
      setState({ kind: "installed" });
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

  return { state, install, dismiss, recheck: runCheck };
}
