import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { LibraryProvider } from "./contexts/LibraryContext";
import { AppLayout } from "./components/layout/AppLayout";

export default function App() {
  return (
    <ThemeProvider>
      <ProfileProvider>
        <LibraryProvider>
          <PlayerProvider>
            <AppLayout />
          </PlayerProvider>
        </LibraryProvider>
      </ProfileProvider>
    </ThemeProvider>
  );
}
