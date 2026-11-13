import { useSyncExternalStore } from "react";

/**
 * The music video on screen (#1073), if any: one at a time, shown by
 * [`ClipStage`](../components/player/ClipStage.tsx) inside the immersive
 * view. A store rather than view state because two places open one — the
 * view's Clip button and a track's context menu — and the menu can name a
 * track that is not the one playing.
 */
export interface OpenClip {
  trackId: number;
  localPath: string;
  title: string;
  artist: string | null;
}

let current: OpenClip | null = null;
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((listener) => listener());
}

export function openClip(clip: OpenClip): void {
  current = clip;
  emit();
}

export function closeClip(): void {
  if (current === null) return;
  current = null;
  emit();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useOpenClip(): OpenClip | null {
  return useSyncExternalStore(
    subscribe,
    () => current,
    () => null,
  );
}
