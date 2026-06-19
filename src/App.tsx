import { ThemeProvider } from "./contexts/ThemeContext";
import { SkinProvider } from "./contexts/SkinContext";
import { PlayerProvider } from "./contexts/PlayerContext";
import { ProfileProvider } from "./contexts/ProfileContext";
import { LibraryProvider } from "./contexts/LibraryContext";
import { PlaylistProvider } from "./contexts/PlaylistContext";
import { SpotifyProvider } from "./contexts/SpotifyContext";
import { AppLayout } from "./components/layout/AppLayout";

export default function App() {
  return (
    // ProfileProvider on the outside so ThemeProvider + SkinProvider can
    // `useProfile()` to scope theme + skin choices per-profile
    // (persisted in `profile_setting['appearance.{theme,skin}.id']`).
    // First-paint stays flash-free because both providers cache the
    // last-applied id in localStorage and read it synchronously at
    // mount; the DB read is async and just confirms / overrides when
    // the active profile differs from the cached one.
    <ProfileProvider>
      <ThemeProvider>
        {/* SkinProvider sits inside ThemeProvider so a future
            theme-aware skin (e.g. a skin that adjusts surface
            contrast for the active theme's mode) can read
            `useTheme()` from inside. Skins themselves don't
            currently depend on themes, but the nesting is the
            cheap-to-keep-right option. */}
        <SkinProvider>
          <LibraryProvider>
            <PlaylistProvider>
              <SpotifyProvider>
                <PlayerProvider>
                  <AppLayout />
                </PlayerProvider>
              </SpotifyProvider>
            </PlaylistProvider>
          </LibraryProvider>
        </SkinProvider>
      </ThemeProvider>
    </ProfileProvider>
  );
}
