import type { ReactNode } from "react";

export type ViewId =
  | "home"
  | "library"
  | "playlist"
  | "settings"
  | "about"
  | "feedback"
  | "statistics"
  | "liked"
  | "recent"
  | "wrapped"
  | "album-detail"
  | "artist-detail"
  | "genre-detail"
  | "year-detail"
  | "web-radio"
  // Standalone podcasts (RFC-009). The channel open, if any, is on the history
  // entry (see AppLayout `HistoryEntry`).
  | "podcasts"
  // The native Release Radar: recent releases of the library's artists.
  | "release-radar"
  // The tag workshop: many files' tags edited at once. Its source is on
  // the history entry (see AppLayout `HistoryEntry`).
  | "tag-workshop"
  // The plugin store and the installed plugins, a page of their own.
  | "extensions"
  // A playlist that lives on the bound remote server (RFC-005 sync_v2).
  // The concrete playlist id is carried on the history entry, like the
  // local "playlist" view. Only reachable in a `sync_v2` build.
  | "remote-playlist"
  // A remote album detail view (RFC-005 sync_v2).
  | "remote-album"
  // A remote artist detail view (RFC-005 sync_v2).
  | "remote-artist"
  // A `ui`-world plugin's custom view. The concrete plugin is carried
  // on the history entry (see AppLayout `HistoryEntry`), not the id.
  | "plugin-ui";

export type LibraryTab =
  | "tracks"
  | "albums"
  | "artists"
  | "genres"
  | "years"
  | "playlists"
  | "folders"
  // The "needs attention" inventory (#589) and the measured quality of
  // the files. Last, because they are the two tabs that answer a
  // question about the library rather than listing part of it.
  | "needs-attention"
  | "quality";

export interface NavItemProps {
  icon?: ReactNode;
  customIcon?: ReactNode;
  label: string;
  subtext?: string;
  active?: boolean;
  onClick?: () => void;
  /** Icon only, for the sidebar's narrow mode: the label stays in the
   *  accessibility tree and shows as the tooltip. */
  collapsed?: boolean;
}

/** The sidebar's three widths, cycled by the top bar's button: full
 *  (256 px), icons only (80 px), hidden. */
export type SidebarMode = "full" | "icons" | "hidden";

export interface TabProps {
  icon: ReactNode;
  label: string;
  active?: boolean;
  onClick?: () => void;
}

export interface IconButtonProps {
  icon: ReactNode;
  className?: string;
  onClick?: () => void;
}

export interface MenuActionItemProps {
  icon: ReactNode;
  label: string;
  danger?: boolean;
  onClick?: () => void;
}
