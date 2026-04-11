import { ThemeProvider } from "./contexts/ThemeContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { AppLayout } from "./components/layout/AppLayout";

export default function App() {
  return (
    <ThemeProvider>
      <PlayerProvider>
        <AppLayout />
      </PlayerProvider>
    </ThemeProvider>
  );
}
