import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { DesktopLyrics } from "./components/views/DesktopLyrics";

/**
 * Provider tree for the floating desktop lyrics window (issue #582).
 *
 * Smaller than the mini-player's: no ThemeProvider or ContrastProvider,
 * because the overlay draws on the desktop in the colours the user
 * picked for it, not in the app theme's. `ProfileProvider` for the
 * style setting, and `PlayerProvider` for the track and the position
 * the lyrics follow.
 */
export function DesktopLyricsApp() {
  return (
    <ProfileProvider>
      <PlayerProvider>
        <DesktopLyrics />
      </PlayerProvider>
    </ProfileProvider>
  );
}
