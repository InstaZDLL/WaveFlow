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

// The glow's colour and strength, read by `.soft-glow` (app.css).
const GLOW_CLASSES: Record<EmptyStateAccent, string> = {
  emerald:
    "[--orb:var(--color-emerald-400)] [--orb-alpha:30%] dark:[--orb:var(--color-emerald-500)] dark:[--orb-alpha:25%]",
  pink: "[--orb:var(--color-pink-400)] [--orb-alpha:30%] dark:[--orb:var(--color-pink-500)] dark:[--orb-alpha:25%]",
  blue: "[--orb:var(--color-blue-400)] [--orb-alpha:30%] dark:[--orb:var(--color-blue-500)] dark:[--orb-alpha:25%]",
};

const ICON_CONTAINER_CLASSES: Record<EmptyStateAccent, string> = {
  emerald: "bg-white text-emerald-500 dark:bg-zinc-800 dark:text-emerald-400",
  pink: "bg-pink-100 text-pink-500 dark:bg-pink-900/30 dark:text-pink-400",
  blue: "bg-blue-100 text-blue-500 dark:bg-blue-900/30 dark:text-blue-400",
};

const SIZE_CLASSES = {
  sm: {
    wrapper: "mb-4",
    icon: "w-20 h-20",
    title: "text-lg font-semibold mb-2 text-zinc-800 dark:text-zinc-200",
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
        {/* A gradient with a blurred disc's profile (`.soft-glow`), not a
            disc under `blur-3xl`: its breathing made the blur run again
            every frame, a whole frame's cost without a GPU. The box is
            the old disc (2.5 rem past the icon) plus twice the blur. */}
        <div
          aria-hidden="true"
          className={`soft-glow absolute -inset-[10.5rem] animate-breathing ${GLOW_CLASSES[accent]}`}
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
