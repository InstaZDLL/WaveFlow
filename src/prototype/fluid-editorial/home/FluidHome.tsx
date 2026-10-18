import type { CSSProperties } from "react";
import { ForYouMosaic } from "./ForYouMosaic";
import { ListeningCanvas } from "./ListeningCanvas";
import { ListeningHistory } from "./ListeningHistory";
import { MoodSpectrum } from "./MoodSpectrum";
import { RecentlyAdded } from "./RecentlyAdded";

const rise = (i: number): CSSProperties => ({ ["--i" as string]: i });

/**
 * The Home page, top to bottom: what you were listening to, what mood
 * you are in, what we picked for you, what is new, what you played.
 * Five sections, five layout families — a split canvas, a band, a
 * mosaic, a shelf, a list.
 */
export function FluidHome() {
  return (
    <div className="mx-auto max-w-[1400px] space-y-14 pb-10">
      <div className="fe-rise" style={rise(0)}>
        <ListeningCanvas />
      </div>
      <div className="fe-rise" style={rise(2)}>
        <MoodSpectrum />
      </div>
      <div className="fe-rise" style={rise(3)}>
        <ForYouMosaic />
      </div>
      <div className="fe-rise" style={rise(4)}>
        <RecentlyAdded />
      </div>
      <div className="fe-rise" style={rise(5)}>
        <ListeningHistory />
      </div>
    </div>
  );
}
