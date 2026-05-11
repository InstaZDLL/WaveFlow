import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { SpotifyProvider } from "./contexts/SpotifyContext";
import { MiniPlayer } from "./components/views/MiniPlayer";

/**
 * Minimal provider tree for the mini-player webview. Skips the
 * Library / Playlist contexts since the mini-player only displays
 * the current track + playback controls — no library browsing.
 *
 * SpotifyProvider stays in because PlayerProvider calls useSpotify()
 * unconditionally (provider routing happens inside PlayerContext).
 * Without it the mini boots into a white screen via the "must be
 * used within SpotifyProvider" throw.
 *
 * The PlayerProvider hooks into the same backend AppState as the
 * main window via tauri events, so playback stays in sync without
 * any bridging code.
 */
export function MiniPlayerApp() {
  return (
    <ThemeProvider>
      <ProfileProvider>
        <SpotifyProvider>
          <PlayerProvider>
            <MiniPlayer />
          </PlayerProvider>
        </SpotifyProvider>
      </ProfileProvider>
    </ThemeProvider>
  );
}
