import type { ReactNode } from "react";

type EmptyStateAccent = "emerald" | "pink" | "blue";
type EmptyStateShape = "square" | "circle";
type EmptyStateSize = "sm" | "md";

interface EmptyStateProps {
  icon: ReactNode;
  title: string;
  description: ReactNode;
  accent?: EmptyStateAccent;
  shape?: EmptyStateShape;
  size?: EmptyStateSize;
  children?: ReactNode;
  className?: string;
}

const GLOW_CLASSES: Record<EmptyStateAccent, string> = {
  emerald: "bg-emerald-400/30 dark:bg-emerald-500/25",
  pink: "bg-pink-400/30 dark:bg-pink-500/25",
  blue: "bg-blue-400/30 dark:bg-blue-500/25",
};

const ICON_CONTAINER_CLASSES: Record<EmptyStateAccent, string> = {
  emerald:
    "bg-white text-emerald-500 dark:bg-zinc-800 dark:text-emerald-400",
  pink: "bg-pink-100 text-pink-500 dark:bg-pink-900/30 dark:text-pink-400",
  blue: "bg-blue-100 text-blue-500 dark:bg-blue-900/30 dark:text-blue-400",
};

const SIZE_CLASSES = {
  sm: {
    wrapper: "mb-4",
    icon: "w-20 h-20",
    title:
      "text-lg font-semibold mb-2 text-zinc-800 dark:text-zinc-200",
    description: "text-sm text-zinc-500 dark:text-zinc-400 max-w-md",
  },
  md: {
    wrapper: "mb-6",
    icon: "w-24 h-24",
    title: "text-xl font-bold mb-2 text-zinc-900 dark:text-white",
    description: "text-zinc-500 dark:text-zinc-400 max-w-sm",
  },
} as const;

export function EmptyState({
  icon,
  title,
  description,
  accent = "emerald",
  shape = "square",
  size = "md",
  children,
  className = "",
}: EmptyStateProps) {
  const shapeClass = shape === "circle" ? "rounded-full" : "rounded-3xl";
  const sizes = SIZE_CLASSES[size];
  const HeadingTag = size === "sm" ? "h3" : "h2";

  return (
    <div
      className={`flex flex-col items-center justify-center text-center ${className}`}
    >
      <div className={`relative ${sizes.wrapper}`}>
        <div
          aria-hidden="true"
          className={`pointer-events-none absolute inset-0 -m-10 rounded-full blur-3xl animate-breathing ${GLOW_CLASSES[accent]}`}
        />
        <div
          className={`relative ${sizes.icon} ${shapeClass} flex items-center justify-center shadow-sm ${ICON_CONTAINER_CLASSES[accent]}`}
        >
          {icon}
        </div>
      </div>

      <HeadingTag className={sizes.title}>{title}</HeadingTag>
      <p className={sizes.description}>{description}</p>

      {children}
    </div>
  );
}
