import { preloadable } from "../../../../lib/preloadable";

// Each Settings page is its own chunk, loaded when it is first opened: the
// cards a page holds are most of what Settings weighs, and a visit reads
// one page. `AppLayout` and `SettingsView` preload them all, so a page
// opened later renders at once.
export const GeneralSection = preloadable(() =>
  import("./GeneralSection").then((m) => m.GeneralSection),
);
export const AppearanceSection = preloadable(() =>
  import("./AppearanceSection").then((m) => m.AppearanceSection),
);
export const AudioSection = preloadable(() =>
  import("./AudioSection").then((m) => m.AudioSection),
);
export const LyricsSection = preloadable(() =>
  import("./LyricsSection").then((m) => m.LyricsSection),
);
export const LibrarySection = preloadable(() =>
  import("./LibrarySection").then((m) => m.LibrarySection),
);
export const IntegrationsSection = preloadable(() =>
  import("./IntegrationsSection").then((m) => m.IntegrationsSection),
);
export const ShortcutsSection = preloadable(() =>
  import("./ShortcutsSection").then((m) => m.ShortcutsSection),
);
export const DataSection = preloadable(() =>
  import("./DataSection").then((m) => m.DataSection),
);
export const DiagnosticsSection = preloadable(() =>
  import("./DiagnosticsSection").then((m) => m.DiagnosticsSection),
);

/** Fetch every page's chunk. Best effort: a chunk that fails here is
 * fetched again when its page opens, which is where an error shows. */
export function preloadSettingsSections() {
  for (const section of [
    GeneralSection,
    AppearanceSection,
    AudioSection,
    LyricsSection,
    LibrarySection,
    IntegrationsSection,
    ShortcutsSection,
    DataSection,
    DiagnosticsSection,
  ]) {
    section.preload().catch(() => {});
  }
}
