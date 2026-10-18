import { useFe } from "../useFe";

/**
 * Two soft pools of the cover's colour behind the content column, plus
 * a grain so the dark never reads as a flat digital slab. Keyed on the
 * glow so a track change cross-fades through React's mount/unmount
 * (the production `SkinAmbientBackdrop` uses the same trick).
 */
export function Atmosphere() {
  const { atmosphere } = useFe();
  return (
    <div className="fe-atmosphere" aria-hidden="true">
      <div key={atmosphere.glow} className="fe-atmosphere__pool" />
      <div className="fe-atmosphere__grain" />
    </div>
  );
}
