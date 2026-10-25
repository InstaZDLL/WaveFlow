import { ThemeProvider } from "./contexts/ThemeContext";
import { AccentSync } from "./components/common/AccentSync";
import { ContrastProvider } from "./contexts/ContrastContext";
import { EffectsProvider } from "./contexts/EffectsContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { MiniPlayer } from "./components/views/MiniPlayer";

/**
 * Minimal provider tree for the mini-player webview. Skips the
 * Library / Playlist contexts since the mini-player only displays
 * the current track + playback controls — no library browsing.
 *
 * ProfileProvider sits OUTSIDE ThemeProvider because the per-profile
 * theme work landed in #264 made `ThemeProvider` call `useProfile()`
 * to scope theme choices per-profile. Without this nesting the
 * mini boots into a white screen via the "must be used within
 * ProfileProvider" throw — mirrors the main `App.tsx` nesting.
 *
 * The PlayerProvider hooks into the same backend AppState as the
 * main window via tauri events, so playback stays in sync without
 * any bridging code.
 */
export function MiniPlayerApp() {
  return (
    <ProfileProvider>
      <ThemeProvider>
        {/* The mini-player is a second WebviewWindow with its own
            document, so it needs its own stamp -- the main window's
            attribute is not shared. Someone who needs high contrast
            needs it in both windows. */}
        <ContrastProvider>
          {/* Its own document again: the effects level too. */}
          <EffectsProvider>
            <PlayerProvider>
              {/* Its own document, so its own accent: a cover accent has
                to follow the track here too. */}
              <AccentSync />
              <MiniPlayer />
            </PlayerProvider>
          </EffectsProvider>
        </ContrastProvider>
      </ThemeProvider>
    </ProfileProvider>
  );
}
