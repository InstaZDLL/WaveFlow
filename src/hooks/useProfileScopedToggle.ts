import { useCallback, useEffect, useRef, useState } from "react";

import { useProfile } from "./useProfile";

/**
 * A boolean setting that lives on the backend, per profile, and is read
 * and written through a pair of Tauri commands rather than through
 * [`useProfileSetting`](./useProfileSetting.ts) — the engine toggles
 * (spectrum visualizer, smart and dynamic crossfade) push their value
 * to the audio threads on the way in, so they own their own commands.
 *
 * What this hook carries, and why each part is there (issue #698):
 *
 * - **The value is tagged with the profile that answered.** Until the
 *   active profile has answered, the setting reads as `hydrated: false`
 *   rather than as the previous profile's value — no reset inside the
 *   effect, which is the id gate [`useArtistImage`](./useArtistImage.ts)
 *   uses.
 * - **The switch stays disabled while `hydrated` is false.** These
 *   handlers derive what they write from what is on screen, so a click
 *   during hydration would persist the previous profile's state into
 *   the new profile.
 * - **A read already in flight never lands on a click.** `touched`
 *   drops the answer once the user has decided.
 * - **A failed write rolls back only if its value is still the one on
 *   screen.** A later click, or a profile switch, has already decided
 *   something else; reverting then would overwrite that decision, or
 *   re-tag the value with a profile we left and leave the control
 *   inert.
 *
 * A failed read resolves to `fallback` rather than leaving the control
 * disabled forever.
 */
export function useProfileScopedToggle(
  read: () => Promise<boolean>,
  write: (value: boolean) => Promise<void>,
  options: { label: string; fallback?: boolean },
): { enabled: boolean; hydrated: boolean; toggle: () => void } {
  const { label, fallback = false } = options;
  const activeProfileId = useProfile().activeProfile?.id;

  const [state, setState] = useState<{
    profileId: number | undefined;
    on: boolean;
  } | null>(null);
  const touched = useRef(false);
  // The read effect is keyed on the profile alone, so the commands are
  // reached through refs: a caller passing an inline arrow would
  // otherwise re-issue the read on every render. Kept in sync from an
  // effect rather than during render, and declared before the read
  // effect so it is the one that runs first on mount.
  const readRef = useRef(read);
  const writeRef = useRef(write);
  useEffect(() => {
    readRef.current = read;
    writeRef.current = write;
  });

  useEffect(() => {
    let stale = false;
    const profileId = activeProfileId;
    touched.current = false;
    readRef
      .current()
      .then((on) => {
        if (stale || touched.current) return;
        setState({ profileId, on });
      })
      .catch((err) => {
        console.error(`[${label}] read failed`, err);
        if (!stale) setState({ profileId, on: fallback });
      });
    return () => {
      stale = true;
    };
  }, [activeProfileId, label, fallback]);

  const hydrated = state !== null && state.profileId === activeProfileId;
  const enabled = hydrated && state.on;

  const toggle = useCallback(() => {
    // The control is disabled until the active profile has answered, so
    // this is the belt to that brace: a caller that renders it enabled
    // anyway must not write a value derived from a profile we left.
    if (!hydrated) return;
    const next = !enabled;
    const profileId = activeProfileId;
    touched.current = true;
    setState({ profileId, on: next });
    writeRef.current(next).catch((err) => {
      console.error(`[${label}] write failed`, err);
      // Undo this write only if it is still the one on screen: a later
      // click, or a profile switch, has already decided something else,
      // and a late rollback would overwrite that decision — or re-tag
      // the value with a profile we left and leave the control inert.
      setState((cur) =>
        cur !== null && cur.profileId === profileId && cur.on === next
          ? { profileId, on: !next }
          : cur,
      );
    });
  }, [enabled, hydrated, label, activeProfileId]);

  return { enabled, hydrated, toggle };
}
