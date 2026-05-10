interface PlayingIndicatorProps {
  isPlaying: boolean;
  className?: string;
}

const BAR_DELAYS = ["-0.6s", "-0.2s", "-0.4s"];

export function PlayingIndicator({
  isPlaying,
  className = "",
}: PlayingIndicatorProps) {
  return (
    <span
      role="img"
      aria-label={isPlaying ? "Playing" : "Paused"}
      className={`inline-flex items-end justify-center gap-[2px] h-3.5 w-3.5 ${className}`}
    >
      {BAR_DELAYS.map((delay, i) => (
        <span
          key={i}
          className="playing-indicator-bar w-[2px] h-full bg-emerald-500 dark:bg-emerald-400 rounded-sm origin-bottom"
          style={{
            animation: isPlaying
              ? `playingBar 0.9s ease-in-out ${delay} infinite`
              : "none",
            transform: isPlaying ? undefined : "scaleY(0.45)",
          }}
        />
      ))}
    </span>
  );
}
