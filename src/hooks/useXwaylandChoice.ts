import { useEffect, useState } from "react";
import {
  displayBackendSetXwayland,
  displayBackendStatus,
  type DisplayBackendStatus,
} from "../lib/tauri/displayBackend";

export interface XwaylandChoice {
  /** `null` until read, and off a Linux Wayland session (`available`
   *  false): the places that show the choice render nothing then. */
  status: DisplayBackendStatus | null;
  busy: boolean;
  error: string | null;
  /** What the switch shows: the running backend when the environment
   *  imposes it, the stored choice otherwise. */
  on: boolean;
  /** Stored and running disagree exactly until the next start, unless the
   *  environment decides either way. */
  pending: boolean;
  toggle: () => void;
}

/**
 * Running through XWayland on a Linux Wayland session (#894), for every
 * place that offers it (Settings → Appearance, the onboarding): the status
 * read once, the choice stored for the next start. Nothing to choose when
 * `GDK_BACKEND` is set by the user's environment.
 */
export function useXwaylandChoice(): XwaylandChoice {
  const [status, setStatus] = useState<DisplayBackendStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    displayBackendStatus()
      .then(setStatus)
      .catch((err) => console.error("[useXwaylandChoice] status failed", err));
  }, []);

  const available = status?.available ? status : null;
  const toggle = () => {
    if (!available || busy || available.set_by_user) return;
    setBusy(true);
    setError(null);
    displayBackendSetXwayland(!available.xwayland)
      .then(setStatus)
      .catch((err) => {
        console.error("[useXwaylandChoice] save failed", err);
        setError(String(err));
      })
      .finally(() => setBusy(false));
  };

  return {
    status: available,
    busy,
    error,
    on: available
      ? available.set_by_user
        ? available.active
        : available.xwayland
      : false,
    pending:
      !!available &&
      !available.set_by_user &&
      available.xwayland !== available.active,
    toggle,
  };
}
