import { useCallback, useEffect, useSyncExternalStore } from "react";
import { useProfileSetting } from "./useProfileSetting";
import {
  applyEffects,
  DEFAULT_EFFECTS_LEVEL,
  EFFECTS_SETTING_KEY,
  effectsReduced,
  launchedInSoftware,
  parseEffectsLevel,
  probeSoftwareGl,
  readCachedProbe,
  resolveEffects,
  writeCachedEffects,
  writeCachedProbe,
  type EffectsLevel,
  type ResolvedEffects,
} from "../lib/effects";
import { rendererStatus } from "../lib/tauri/renderer";

/** Broadcast after a successful write so the Settings card and the
 *  provider that paints the attribute re-read together. */
export const EFFECTS_EVENT = "waveflow:effects";

export interface EffectsPreference {
  level: EffectsLevel;
  /** What `auto` resolves to on this machine, whatever the choice. */
  auto: ResolvedEffects;
  ready: boolean;
  setLevel: (next: EffectsLevel) => Promise<void>;
}

const parse = (raw: string | null) => parseEffectsLevel(raw);
const serialize = (value: EffectsLevel) => value;

// Whether this window draws without a GPU: one answer per window, shared
// by every consumer. Starts from what is known synchronously (the
// launch's URL, the last probe), then the renderer status (asked at
// once) and a fresh probe (after the first paint) settle it.
let noGpu = launchedInSoftware() || readCachedProbe();
let settled = false;
const listeners = new Set<() => void>();

function settle() {
  if (settled) return;
  settled = true;
  const set = (value: boolean) => {
    if (value === noGpu) return;
    noGpu = value;
    listeners.forEach((l) => l());
  };
  // Already known from the URL: no context to create, nothing to ask.
  if (launchedInSoftware()) return set(true);
  let software = false;
  // The last probe's answer until this one's: a status that lands first
  // must not undo it for the second and a half in between.
  let softwareGl = readCachedProbe();
  // The status is the backend's, ready before any window exists.
  rendererStatus()
    .then((status) => {
      software = status?.mode === "software";
      set(software || softwareGl);
    })
    .catch(() => {});
  // The probe after the first paint: a WebGL context is not free to
  // create, and nothing on screen waits for its answer.
  window.setTimeout(() => {
    const probe = probeSoftwareGl();
    if (probe !== null) writeCachedProbe(probe);
    softwareGl = probe ?? false;
    set(software || softwareGl);
  }, 1500);
}

function subscribeNoGpu(listener: () => void) {
  settle();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

const getNoGpu = () => noGpu;

/**
 * Per-profile preference: how much the interface draws besides its
 * content (`lib/effects.ts`).
 *
 * Same shape as [`useContrastMode`](./useContrastMode.ts): persistence
 * through `useProfileSetting`, then the two side effects outside React
 * — the `data-effects` attribute on the root and the cache the
 * `index.html` bootstrap reads — painted only once the stored choice is
 * known, so the bootstrap's stamp is never overwritten by the default.
 */
export function useEffectsLevel(): EffectsPreference {
  const { value, ready, revision, setValue } = useProfileSetting<EffectsLevel>({
    key: EFFECTS_SETTING_KEY,
    defaultValue: DEFAULT_EFFECTS_LEVEL,
    parse,
    serialize,
    valueType: "string",
    event: EFFECTS_EVENT,
    label: "useEffectsLevel",
  });
  const software = useSyncExternalStore(subscribeNoGpu, getNoGpu, getNoGpu);

  useEffect(() => {
    if (!ready) return;
    applyEffects(resolveEffects(value, software));
    writeCachedEffects(value);
  }, [ready, value, software, revision]);

  const setLevel = useCallback(
    async (next: EffectsLevel) => {
      applyEffects(resolveEffects(next, noGpu));
      await setValue(next);
    },
    [setValue],
  );

  return {
    level: value,
    auto: resolveEffects("auto", software),
    ready,
    setLevel,
  };
}

function subscribeAttribute(listener: () => void) {
  const observer = new MutationObserver(listener);
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-effects"],
  });
  return () => observer.disconnect();
}

/**
 * Whether the document is drawn with reduced effects, for a component
 * that has to leave something out rather than restyle it (Lounge's
 * room). Reads the attribute the provider stamps, so it follows a change
 * in Settings without a database read of its own.
 */
export function useReducedEffects(): boolean {
  return useSyncExternalStore(subscribeAttribute, effectsReduced, () => false);
}
