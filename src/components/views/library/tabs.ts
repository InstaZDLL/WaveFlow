import { preloadable } from "../../../lib/preloadable";

// What a Library tab shows is its own chunk, loaded when the tab is first
// opened: a visit reads one tab, and the others are most of what the view
// weighs. `AppLayout` and `LibraryView` preload them all, so a tab opened
// later renders at once.
export const TrackTable = preloadable(() =>
  import("./TrackTable").then((m) => m.TrackTable),
);
export const AlbumGrid = preloadable(() =>
  import("./AlbumGrid").then((m) => m.AlbumGrid),
);
export const ArtistList = preloadable(() =>
  import("./ArtistList").then((m) => m.ArtistList),
);
export const YearList = preloadable(() =>
  import("./YearList").then((m) => m.YearList),
);
export const GenreList = preloadable(() =>
  import("./GenreList").then((m) => m.GenreList),
);
export const FolderList = preloadable(() =>
  import("./FolderList").then((m) => m.FolderList),
);
export const FolderBrowser = preloadable(() =>
  import("./FolderBrowser").then((m) => m.FolderBrowser),
);
export const PlaylistGrid = preloadable(() =>
  import("./PlaylistGrid").then((m) => m.PlaylistGrid),
);
export const InventoryCategories = preloadable(() =>
  import("./InventoryCategories").then((m) => m.InventoryCategories),
);
export const PhantomArtistList = preloadable(() =>
  import("./PhantomArtistList").then((m) => m.PhantomArtistList),
);

/** Fetch every tab's chunk. Best effort: a chunk that fails here is
 * fetched again when its tab opens, which is where an error shows. */
export function preloadLibraryTabs() {
  for (const tab of [
    TrackTable,
    AlbumGrid,
    ArtistList,
    YearList,
    GenreList,
    FolderList,
    FolderBrowser,
    PlaylistGrid,
    InventoryCategories,
    PhantomArtistList,
  ]) {
    tab.preload().catch(() => {});
  }
}
