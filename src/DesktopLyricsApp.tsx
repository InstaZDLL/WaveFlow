import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { SpotifyProvider } from "./contexts/SpotifyContext";
import { DesktopLyrics } from "./components/views/DesktopLyrics";

/**
 * Provider tree for the floating desktop lyrics window (issue #582).
 *
 * Smaller than the mini-player's: no ThemeProvider or ContrastProvider,
 * because the overlay draws on the desktop in the colours the user
 * picked for it, not in the app theme's. `ProfileProvider` for the
 * style setting, `SpotifyProvider` because `PlayerProvider` calls
 * `useSpotify()` unconditionally (it stays out of the SDK here, see
 * `IS_SECONDARY_WINDOW`), and `PlayerProvider` for the track and the
 * position the lyrics follow.
 */
export function DesktopLyricsApp() {
  return (
    <ProfileProvider>
      <SpotifyProvider>
        <PlayerProvider>
          <DesktopLyrics />
        </PlayerProvider>
      </SpotifyProvider>
    </ProfileProvider>
  );
}
