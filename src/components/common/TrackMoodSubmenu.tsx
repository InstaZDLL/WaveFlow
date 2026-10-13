import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Smile } from "lucide-react";
import {
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
} from "./ContextMenu";
import {
  MOODS,
  setMoodOverride,
  trackMoodOverrides,
  type Mood,
  type MoodOverrideKind,
} from "../../lib/tauri/moodRadio";

interface TrackMoodSubmenuProps {
  trackId: number;
  onClose: () => void;
}

/**
 * "Moods" submenu of the track menu: put the track into a mood radio
 * or take it out of one, by hand. The tempo and loudness gates cannot
 * tell every slow ballad from a rock track the analysis read at 60 BPM,
 * so the user has the last word. A tick marks where the track already
 * stands; choosing a ticked row hands the track back to the gates.
 */
export function TrackMoodSubmenu({ trackId, onClose }: TrackMoodSubmenuProps) {
  const { t } = useTranslation();
  const [state, setState] = useState<Partial<Record<Mood, MoodOverrideKind>>>(
    {},
  );

  useEffect(() => {
    let cancelled = false;
    trackMoodOverrides(trackId)
      .then((rows) => {
        if (cancelled) return;
        setState(Object.fromEntries(rows.map((r) => [r.mood, r.kind])));
      })
      .catch((err) =>
        console.error("[TrackMoodSubmenu] overrides failed", err),
      );
    return () => {
      cancelled = true;
    };
  }, [trackId]);

  const choose = (mood: Mood, kind: MoodOverrideKind) => () => {
    const next = state[mood] === kind ? null : kind;
    setMoodOverride(mood, [trackId], next).catch((err) =>
      console.error("[TrackMoodSubmenu] set override failed", err),
    );
    onClose();
  };

  const group = (kind: MoodOverrideKind) =>
    MOODS.map((mood) => (
      <ContextMenuItem
        key={`${kind}-${mood}`}
        icon={
          state[mood] === kind ? (
            <Check size={14} className="text-emerald-500" />
          ) : (
            <span className="w-3.5" />
          )
        }
        label={t(`home.moodRadio.${mood}.title`)}
        onSelect={choose(mood, kind)}
      />
    ));

  return (
    <ContextMenuSub icon={<Smile size={14} />} label={t("trackActions.moods")}>
      <div className="px-3 pt-2 pb-1 text-[11px] font-semibold uppercase tracking-wide text-zinc-400">
        {t("trackActions.moodAddTo")}
      </div>
      {group("include")}
      <ContextMenuSeparator />
      <div className="px-3 pt-1 pb-1 text-[11px] font-semibold uppercase tracking-wide text-zinc-400">
        {t("trackActions.moodRemoveFrom")}
      </div>
      {group("exclude")}
    </ContextMenuSub>
  );
}
