import { useMemo } from "react";

interface Props {
  items: { name: string }[];
  onLetterClick: (firstIndex: number) => void;
  className?: string;
}

const LETTERS: string[] = [
  "#",
  ..."ABCDEFGHIJKLMNOPQRSTUVWXYZ".split(""),
];

function firstLetterKey(name: string): string {
  // NFD-normalize then strip combining marks so "Émilie" buckets under "E".
  const stripped = name
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .trim()
    .toUpperCase();
  if (stripped.length === 0) return "#";
  const c = stripped.charAt(0);
  if (c >= "A" && c <= "Z") return c;
  return "#";
}

export function AlphabetIndex({ items, onLetterClick, className = "" }: Props) {
  const letterToIndex = useMemo(() => {
    const map = new Map<string, number>();
    for (let i = 0; i < items.length; i++) {
      const key = firstLetterKey(items[i].name);
      if (!map.has(key)) map.set(key, i);
    }
    return map;
  }, [items]);

  return (
    <div
      role="navigation"
      aria-label="Alphabet index"
      className={`flex flex-col items-center select-none ${className}`}
    >
      {LETTERS.map((letter) => {
        const idx = letterToIndex.get(letter);
        const isPresent = idx !== undefined;
        return (
          <button
            key={letter}
            type="button"
            disabled={!isPresent}
            onClick={() => {
              if (idx !== undefined) onLetterClick(idx);
            }}
            aria-label={letter}
            className={`text-[10px] font-bold leading-tight px-1 transition-colors ${
              isPresent
                ? "text-zinc-500 hover:text-blue-400 dark:text-zinc-400 dark:hover:text-blue-400 cursor-pointer"
                : "opacity-30 pointer-events-none text-zinc-400 dark:text-zinc-600"
            }`}
          >
            {letter}
          </button>
        );
      })}
    </div>
  );
}
