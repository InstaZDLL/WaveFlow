import { listTracks, type Track } from "../../lib/tauri/track";

/** Every track of the library, in a random order: what "play something"
 *  from the onboarding and the first steps hands the player. */
export async function shuffledLibrary(): Promise<Track[]> {
  const tracks = await listTracks(null);
  for (let i = tracks.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [tracks[i], tracks[j]] = [tracks[j], tracks[i]];
  }
  return tracks;
}
