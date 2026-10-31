import { useCallback, useEffect, useRef, useState } from "react";

import { useProfile } from "./useProfile";

/**
 * A boolean setting that lives on the backend, per profile, and is read
 * and written through a pair of Tauri commands rather than through
 * [`useProfileSetting`](./useProfileSetting.ts) — the engine toggles
 * (spectrum visualizer, smart and dynamic crossfade, AutoMix) push their value
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
 * - **Writes go one at a time, and the last click wins** (issue #995).
 *   Each write waits for the one before it, so two fast clicks can no
 *   longer reach the backend in the wrong order and leave the first
 *   value saved; a queued write that a later click has superseded is
 *   skipped, the later one carrying the value the user ended on. A
 *   failed write does not stop the chain.
 * - **A queued write belongs to the profile it was made in.** It is not
 *   sent once that profile is no longer active, and when it is sent it
 *   names that profile to the command, which refuses it if a switch got
 *   in after this check (issue #996) — only the backend can close that
 *   window, the check here runs before the IPC hop.
 * - **A failed write rolls back only while it is still the last
 *   decision**, and to the value the backend last confirmed. Each click
 *   and each profile change bumps a revision; a failure from an older
 *   one is dropped, because a later click has already decided something
 *   else — and comparing values instead would miss the case where three
 *   clicks bring the switch back to what the failed write was setting.
 *
 * A failed read resolves to `fallback` rather than leaving the control
 * disabled forever.
 */
export function useProfileScopedToggle(
  read: () => Promise<boolean>,
  write: (value: boolean, expectedProfileId: number | null) => Promise<void>,
  options: { label: string; fallback?: boolean },
): { enabled: boolean; hydrated: boolean; toggle: () => void } {
  const { label, fallback = false } = options;
  const activeProfileId = useProfile().activeProfile?.id;

  const [state, setState] = useState<{
    profileId: number | undefined;
    on: boolean;
  } | null>(null);
  const touched = useRef(false);
  // Bumped by every click and by every profile change. A write that
  // fails belongs to one revision; anything newer has already decided.
  const revision = useRef(0);
  // Bumped by clicks alone: a queued write is skipped once a later click
  // has queued its own.
  const clicks = useRef(0);
  // The profile active now, for a write that waited in the chain to
  // check it still belongs to the profile on screen.
  const liveProfileId = useRef(activeProfileId);
  // What the backend last held for the profile on screen: the read's
  // answer, then each write it accepted. A failed write rolls back to
  // it, not to the value its own click replaced — after a queue of
  // clicks, that one may never have reached the backend.
  const confirmed = useRef<{ profileId: number | undefined; on: boolean }>({
    profileId: undefined,
    on: fallback,
  });
  // The writes, one after the other. Never rejects: every link catches
  // its own failure.
  const chain = useRef<Promise<void>>(Promise.resolve());
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
    liveProfileId.current = profileId;
    touched.current = false;
    revision.current += 1;
    readRef
      .current()
      .then((on) => {
        // Once the user has clicked, the writes are what the backend
        // holds: an answer read before them must not stand in for them.
        if (stale || touched.current) return;
        confirmed.current = { profileId, on };
        setState({ profileId, on });
      })
      .catch((err) => {
        console.error(`[${label}] read failed`, err);
        if (stale || touched.current) return;
        confirmed.current = { profileId, on: fallback };
        setState({ profileId, on: fallback });
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
    revision.current += 1;
    const writeRevision = revision.current;
    clicks.current += 1;
    const click = clicks.current;
    setState({ profileId, on: next });
    chain.current = chain.current.then(async () => {
      // Superseded while it waited: a later click is queued behind this
      // one and carries the value the user ended on.
      if (clicks.current !== click) return;
      // Made in a profile that is no longer active: not this one's to
      // save. The command would refuse it anyway; this spares the trip.
      if (liveProfileId.current !== profileId) return;
      try {
        await writeRef.current(next, profileId ?? null);
        if (liveProfileId.current === profileId) {
          confirmed.current = { profileId, on: next };
        }
      } catch (err) {
        console.error(`[${label}] write failed`, err);
        // Undo this write only while it is still the last decision made.
        // Comparing the value instead would not be enough: three clicks
        // bring the switch back to what this write was setting, and the
        // rollback would then undo the click the user actually ended on.
        // A profile change bumps the revision too, so a late failure
        // cannot re-tag the value with a profile we have left.
        if (revision.current !== writeRevision) return;
        const held = confirmed.current;
        setState((cur) =>
          cur !== null &&
          cur.profileId === profileId &&
          held.profileId === profileId
            ? { profileId, on: held.on }
            : cur,
        );
      }
    });
  }, [enabled, hydrated, label, activeProfileId]);

  return { enabled, hydrated, toggle };
}
