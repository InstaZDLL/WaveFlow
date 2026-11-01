import { useEffect, useState } from "react";
import { useProfile } from "../../hooks/useProfile";
import { markTipSeen, tipSeen, type TipId } from "../../lib/discovery";

/**
 * Whether a tip is to be drawn for the active profile. "Once" is counted
 * when it is shown, as the Release Radar notice counts it: the flag is
 * written as soon as the tip is drawn, and the tip stays up until its
 * place unmounts or it is closed. A tip that waited for a click would
 * come back every visit for someone who never closed it.
 *
 * `when` is the context the tip belongs to (an empty lyrics panel, an
 * open editor); the flag is only read once it holds.
 */
export function useTip(id: TipId, when: boolean) {
  const profileId = useProfile().activeProfile?.id ?? null;
  // The profile the tip was decided for, and whether it shows there.
  const [decided, setDecided] = useState<{
    profileId: number;
    show: boolean;
  } | null>(null);

  useEffect(() => {
    if (profileId == null) return;
    if (!when) {
      // Out of its context: the next time the context holds, the flag is
      // read again — and found written, so the tip stays away.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setDecided(null);
      return;
    }
    if (decided?.profileId === profileId) return;
    let cancelled = false;
    tipSeen(id, profileId)
      .then((seen) => {
        if (cancelled) return;
        setDecided({ profileId, show: !seen });
        if (!seen) {
          markTipSeen(id, profileId).catch((err) =>
            console.error(`[ContextTip] mark ${id} failed`, err),
          );
        }
      })
      .catch((err) => console.error(`[ContextTip] read ${id} failed`, err));
    return () => {
      cancelled = true;
    };
  }, [id, when, profileId, decided]);

  const visible =
    when && decided != null && decided.profileId === profileId && decided.show;
  const dismiss = () =>
    setDecided((cur) => (cur ? { ...cur, show: false } : cur));
  return { visible, dismiss };
}
