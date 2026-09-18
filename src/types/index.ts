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
  | "web-radio"
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
  | "playlists"
  | "folders"
  // The "needs attention" inventory (#589). Last, because it is
  // the only tab that answers a question about the library rather
  // than listing part of it.
  | "needs-attention";

export interface NavItemProps {
  icon?: ReactNode;
  customIcon?: ReactNode;
  label: string;
  subtext?: string;
  active?: boolean;
  onClick?: () => void;
}

export type StatCardAccent = "emerald" | "pink" | "blue" | "purple";

export interface StatCardProps {
  icon: ReactNode;
  accent: StatCardAccent;
  count: string;
  label: string;
  onClick?: () => void;
}

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

export interface ActionLinkProps {
  icon: ReactNode;
  label: string;
  highlight?: boolean;
  onClick?: () => void;
}
