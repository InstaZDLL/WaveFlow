import { useCallback, useEffect, useRef, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

/**
 * One implementation of the per-profile preference pattern, shared by
 * every `profile_setting`-backed hook (issue #485).
 *
 * It used to be copy-pasted per preference, and the copies drifted:
 * some had a readiness flag, some rolled back to the optimistic value
 * instead of the confirmed one, only one invalidated a stale read. The
 * guarantees below are the union of the best of each, so a new
 * preference gets all of them for free:
 *
 * - **Optimistic UI.** `setValue` updates the render state and a
 *   synchronous ref immediately; persistence happens behind it.
 * - **Serialized writes.** Each write waits for the previous one, so
 *   two rapid toggles land in click order and the last user action wins
 *   the final DB state.
 * - **Cross-invalidation.** Reads and writes share one monotonic token.
 *   A read applies its result only while it's still the newest thing
 *   that happened, so a read in flight can't clobber a toggle fired
 *   meanwhile — and only the newest write broadcasts.
 * - **Rollback to confirmed truth.** A failed write reverts to the last
 *   value the backend acknowledged, not to the pre-toggle optimistic
 *   value, which may itself never have been persisted after a run of
 *   failures. It only reverts when nothing newer replaced the value it
 *   installed (identity comparison).
 * - **Profile isolation.** A switch resets the value to the default and
 *   drops `ready` before reading the new profile, so the outgoing
 *   profile's preference never paints — even if that read then fails.
 *   Both IPC calls carry the captured profile id, so the backend
 *   refuses a queued write whose profile changed under it instead of
 *   writing it into the new one (a JS-side check can't cover that gap;
 *   it runs before the IPC hop).
 * - **Cross-consumer sync.** A successful write broadcasts `event`, and
 *   every mounted instance re-reads.
 *
 * `parse`, `serialize` and `defaultValue` must be stable across renders
 * — declare them at module level. They're held in refs, so changing
 * their identity mid-life is ignored rather than re-running the read.
 */
export interface ProfileSettingOptions<T> {
  /** `profile_setting` key, e.g. `"ui.artist_hero"`. */
  key: string;
  /** Value used before the first read lands, and after a profile switch. */
  defaultValue: T;
  /** Turn the stored string (or `null` when unset) into a value. */
  parse: (raw: string | null) => T;
  /** Turn a value into the string to persist. */
  serialize: (value: T) => string;
  /** Typed marker stored alongside the raw string. */
  valueType: "bool" | "int" | "string" | "json";
  /** Window event broadcast after a successful write. */
  event: string;
  /** Prefix for console diagnostics, e.g. `"useArtistHero"`. */
  label: string;
}

export interface ProfileSetting<T> {
  value: T;
  /**
   * `false` until the active profile's value has been read. Consumers
   * whose default would paint something (a backdrop, a hidden card)
   * gate on this so nothing flashes before the preference loads.
   */
  ready: boolean;
  /**
   * Optimistic update. Accepts a value or an updater, which receives the
   * synchronously-current value so back-to-back calls each build on the
   * previous one rather than on a render-lagged snapshot.
   *
   * Never rejects: failures are logged and rolled back internally.
   */
  setValue: (next: T | ((previous: T) => T)) => Promise<void>;
}

/** Parse a persisted boolean. Anything other than the two truthy forms
 *  reads as false; a missing key falls back to the caller's default. */
export function parseBoolean(raw: string | null, fallback: boolean): boolean {
  if (raw == null) return fallback;
  return raw === "true" || raw === "1";
}

/**
 * [`useProfileSetting`] specialised for the on/off preferences that make
 * up most of Settings → Appearance. Same guarantees, minus the
 * parse/serialize boilerplate every boolean was repeating.
 */
export function useProfileBooleanSetting(options: {
  key: string;
  defaultValue: boolean;
  event: string;
  label: string;
}): ProfileSetting<boolean> {
  const { key, defaultValue, event, label } = options;
  return useProfileSetting<boolean>({
    key,
    defaultValue,
    // Defined inline but read through a ref by the hook, so the fresh
    // identity on each render is harmless — see the options contract.
    parse: (raw) => parseBoolean(raw, defaultValue),
    serialize: (value) => (value ? "true" : "false"),
    valueType: "bool",
    event,
    label,
  });
}

export function useProfileSetting<T>(
  options: ProfileSettingOptions<T>,
): ProfileSetting<T> {
  // `label`, `parse`, `serialize` and `defaultValue` are read through
  // `optionsRef` instead — they must not enter a dependency array.
  const { key, valueType, event } = options;
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;

  // Stable-by-contract inputs, held in refs so their identity never
  // feeds a dependency array (a caller passing an inline arrow would
  // otherwise re-run the read on every render).
  const optionsRef = useRef(options);
  // Refreshed after every render rather than during it (refs are not
  // render-time state). Declared before the read effect below so that
  // effect always sees the latest — and by contract these values don't
  // change anyway.
  useEffect(() => {
    optionsRef.current = options;
  });

  const [value, setValueState] = useState<T>(options.defaultValue);
  const [ready, setReady] = useState(false);

  // Authoritative copy read synchronously by `setValue` — React state
  // lags a render behind, so two rapid updates would otherwise both
  // branch off the same stale snapshot.
  const valueRef = useRef<T>(options.defaultValue);
  const commit = useCallback((next: T) => {
    valueRef.current = next;
    setValueState(next);
  }, []);

  // Last value the backend acknowledged — the rollback target.
  const confirmedRef = useRef<T>(options.defaultValue);
  const writeChainRef = useRef<Promise<unknown>>(Promise.resolve());
  // Who owns the value: bumped by every read AND every write, so
  // whoever acted last wins and stale results drop themselves.
  const ownerSeqRef = useRef(0);
  // Who broadcasts: bumped by writes only. Kept separate from
  // `ownerSeqRef` because a read landing mid-write must invalidate that
  // write's *value*, not silence its broadcast — other consumers still
  // need to hear about it.
  const writeSeqRef = useRef(0);
  const activeProfileIdRef = useRef<number | null>(activeProfileId);
  useEffect(() => {
    activeProfileIdRef.current = activeProfileId;
  }, [activeProfileId]);

  useEffect(() => {
    let cancelled = false;
    const { defaultValue, parse } = optionsRef.current;
    // Start each profile from a clean slate so the outgoing profile's
    // value can't paint while the new read is in flight — nor stay if
    // that read fails.
    /* eslint-disable react-hooks/set-state-in-effect */
    setReady(false);
    commit(defaultValue);
    /* eslint-enable react-hooks/set-state-in-effect */
    confirmedRef.current = defaultValue;

    const refresh = async () => {
      const seq = ++ownerSeqRef.current;
      try {
        const raw = await getProfileSetting(key, activeProfileId);
        // Stale: a write (or a newer read) happened while we awaited and
        // owns the value now.
        if (cancelled || ownerSeqRef.current !== seq) return;
        const parsed = parse(raw);
        commit(parsed);
        confirmedRef.current = parsed;
      } catch (err) {
        console.error(`[${optionsRef.current.label}] read failed`, err);
      } finally {
        // Ready either way: a failed read leaves the default in place,
        // and never flipping this would gate the consumer forever.
        if (!cancelled) setReady(true);
      }
    };
    void refresh();
    window.addEventListener(event, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(event, refresh);
    };
  }, [activeProfileId, key, event, commit]);

  const setValue = useCallback(
    async (next: T | ((previous: T) => T)) => {
      const resolved =
        typeof next === "function"
          ? (next as (previous: T) => T)(valueRef.current)
          : next;
      commit(resolved);
      const profileAtWrite = activeProfileIdRef.current;
      const ownerSeq = ++ownerSeqRef.current;
      const writeSeq = ++writeSeqRef.current;
      const write = writeChainRef.current
        // Leading no-op catch: a prior failure must not break the chain
        // for later writes.
        .catch(() => {})
        .then(async () => {
          await setProfileSetting(
            key,
            optionsRef.current.serialize(resolved),
            valueType,
            // The backend re-checks this under the switch lock; the JS
            // guard below is only an early out that saves an IPC hop.
            profileAtWrite,
          );
          if (activeProfileIdRef.current !== profileAtWrite) return;
          confirmedRef.current = resolved;
          // Only the newest write broadcasts — an older one completing
          // late must not make everyone re-read over a newer state.
          if (writeSeq === writeSeqRef.current) {
            window.dispatchEvent(new CustomEvent(event));
          }
        });
      writeChainRef.current = write;
      try {
        await write;
      } catch (err) {
        console.error(`[${optionsRef.current.label}] write failed`, err);
        if (activeProfileIdRef.current !== profileAtWrite) return;
        // Roll back only when nothing newer took ownership. Comparing
        // the token rather than the value itself is what makes this work
        // for primitives, where two writes of `true` are indistinguishable.
        if (ownerSeq === ownerSeqRef.current) commit(confirmedRef.current);
      }
    },
    [commit, key, valueType, event],
  );

  return { value, ready, setValue };
}
