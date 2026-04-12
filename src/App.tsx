import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { LibraryProvider } from "./contexts/LibraryContext";
import { PlaylistProvider } from "./contexts/PlaylistContext";
import { AppLayout } from "./components/layout/AppLayout";

export default function App() {
  return (
    <ThemeProvider>
      <ProfileProvider>
        <LibraryProvider>
          <PlaylistProvider>
            <PlayerProvider>
              <AppLayout />
            </PlayerProvider>
          </PlaylistProvider>
        </LibraryProvider>
      </ProfileProvider>
    </ThemeProvider>
  );
}
