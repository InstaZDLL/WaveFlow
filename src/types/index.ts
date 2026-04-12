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
  | "album-detail"
  | "artist-detail";

export type LibraryTab = "morceaux" | "albums" | "artistes" | "genres" | "dossiers";

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
