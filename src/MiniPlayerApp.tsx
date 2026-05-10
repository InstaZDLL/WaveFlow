import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { MiniPlayer } from "./components/views/MiniPlayer";

/**
 * Minimal provider tree for the mini-player webview. Skips the
 * Library / Playlist contexts since the mini-player only displays
 * the current track + playback controls — no library browsing.
 *
 * The PlayerProvider hooks into the same backend AppState as the
 * main window via tauri events, so playback stays in sync without
 * any bridging code.
 */
export function MiniPlayerApp() {
  return (
    <ThemeProvider>
      <ProfileProvider>
        <PlayerProvider>
          <MiniPlayer />
        </PlayerProvider>
      </ProfileProvider>
    </ThemeProvider>
  );
}
