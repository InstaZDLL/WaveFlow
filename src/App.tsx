import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { LibraryProvider } from "./contexts/LibraryContext";
import { PlaylistProvider } from "./contexts/PlaylistContext";
import { SpotifyProvider } from "./contexts/SpotifyContext";
import { AppLayout } from "./components/layout/AppLayout";

export default function App() {
  return (
    <ThemeProvider>
      <ProfileProvider>
        <LibraryProvider>
          <PlaylistProvider>
            <SpotifyProvider>
              <PlayerProvider>
                <AppLayout />
              </PlayerProvider>
            </SpotifyProvider>
          </PlaylistProvider>
        </LibraryProvider>
      </ProfileProvider>
    </ThemeProvider>
  );
}
