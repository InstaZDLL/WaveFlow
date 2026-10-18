import { useProfileSetting, type ProfileSetting } from "./useProfileSetting";
import {
  DEFAULT_SUGGESTION_DRAW,
  SUGGESTION_DRAW_KEY,
  parseSuggestionDraw,
  serializeSuggestionDraw,
  type SuggestionDraw,
} from "../lib/tauri/homeSuggestion";

/** Broadcast after a write, so every mounted card follows the same draw. */
export const SUGGESTION_DRAW_EVENT = "waveflow:suggestion-draw";

/**
 * Per-profile record of Home's suggestion draw, so "Another suggestion"
 * holds for the rest of the day instead of resetting on every visit.
 */
export function useSuggestionDraw(): ProfileSetting<SuggestionDraw> {
  return useProfileSetting<SuggestionDraw>({
    key: SUGGESTION_DRAW_KEY,
    defaultValue: DEFAULT_SUGGESTION_DRAW,
    parse: parseSuggestionDraw,
    serialize: serializeSuggestionDraw,
    valueType: "json",
    event: SUGGESTION_DRAW_EVENT,
    label: "useSuggestionDraw",
  });
}
