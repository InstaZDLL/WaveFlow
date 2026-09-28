import { useProfileBooleanSetting } from "./useProfileSetting";

/** Broadcast after a write, so every mounted lyrics view re-reads. */
export const HELD_NOTE_GLOW_EVENT = "waveflow:lyrics-held-notes-changed";

/**
 * Whether a held note ripples through its letters in the immersive
 * lyrics, per profile in `profile_setting['lyrics.held_notes']`. Default
 * on: it only moves on words the document times as held, and the system's
 * reduced-motion preference already turns it off.
 */
export function useHeldNoteGlowSetting() {
  return useProfileBooleanSetting({
    key: "lyrics.held_notes",
    defaultValue: true,
    event: HELD_NOTE_GLOW_EVENT,
    label: "useHeldNoteGlowSetting",
  });
}

/** Just the value, for the lyrics views. */
export function useHeldNoteGlow(): boolean {
  return useHeldNoteGlowSetting().value;
}
