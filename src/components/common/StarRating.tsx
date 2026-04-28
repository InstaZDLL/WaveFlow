import { useState } from "react";
import { Star } from "lucide-react";

interface StarRatingProps {
  value: number | null;
  onChange?: (rating: number | null) => void;
  readOnly?: boolean;
  size?: "sm" | "md";
}

const SIZE_PX = { sm: 14, md: 18 } as const;

function popmToScore(popm: number | null): number {
  if (popm == null) return 0;
  return Math.round((popm / 255) * 10) / 2;
}

function scoreToPopm(score: number): number {
  return Math.round((score / 5) * 255);
}

export function StarRating({
  value,
  onChange,
  readOnly = false,
  size = "sm",
}: StarRatingProps) {
  const [hoverScore, setHoverScore] = useState<number | null>(null);
  const baseScore = popmToScore(value);
  const displayScore = hoverScore ?? baseScore;
  const px = SIZE_PX[size];

  const interactive = !readOnly && onChange != null;

  const handleClick = (
    event: React.MouseEvent<HTMLButtonElement>,
    starIndex: number,
  ) => {
    if (!interactive) return;
    event.stopPropagation();
    const rect = event.currentTarget.getBoundingClientRect();
    const isLeftHalf = event.clientX - rect.left < rect.width / 2;
    const score = starIndex + (isLeftHalf ? 0.5 : 1);
    onChange!(scoreToPopm(score));
  };

  const handleContextMenu = (event: React.MouseEvent<HTMLDivElement>) => {
    if (!interactive) return;
    event.preventDefault();
    event.stopPropagation();
    onChange!(null);
  };

  const handleHoverStar = (
    event: React.MouseEvent<HTMLButtonElement>,
    starIndex: number,
  ) => {
    if (!interactive) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const isLeftHalf = event.clientX - rect.left < rect.width / 2;
    setHoverScore(starIndex + (isLeftHalf ? 0.5 : 1));
  };

  return (
    <div
      className="inline-flex items-center"
      onMouseLeave={() => setHoverScore(null)}
      onContextMenu={handleContextMenu}
    >
      {[0, 1, 2, 3, 4].map((i) => {
        const filled = displayScore >= i + 1;
        const half = !filled && displayScore >= i + 0.5;
        return (
          <button
            key={i}
            type="button"
            disabled={!interactive}
            onClick={(e) => handleClick(e, i)}
            onMouseMove={(e) => handleHoverStar(e, i)}
            aria-label={`${i + 1}`}
            className={`relative inline-flex p-0.5 ${
              interactive ? "cursor-pointer" : "cursor-default"
            }`}
            tabIndex={interactive ? 0 : -1}
          >
            <Star
              size={px}
              className={
                filled
                  ? "fill-yellow-400 text-yellow-400"
                  : "text-zinc-300 dark:text-zinc-600"
              }
            />
            {half && (
              <span
                className="absolute inset-0 p-0.5 overflow-hidden pointer-events-none"
                style={{ width: "50%" }}
              >
                <Star size={px} className="fill-yellow-400 text-yellow-400" />
              </span>
            )}
          </button>
        );
      })}
    </div>
  );
}
