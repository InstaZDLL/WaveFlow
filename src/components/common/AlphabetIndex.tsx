import { useMemo } from "react";

import { useSortIgnoreArticles } from "../../hooks/useSortIgnoreArticles";
import { stripArticle } from "../../lib/sortName";

interface Props {
  items: { name: string }[];
  onLetterClick: (firstIndex: number) => void;
  className?: string;
}

const LETTERS: string[] = ["#", ..."ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("")];

function firstLetterKey(name: string, ignoreArticles: boolean): string {
  // The letter the listing sorted the name under: without its article
  // when the backend set it aside ("The Beatles" under B), then
  // NFD-normalized and stripped of combining marks so "Émilie" buckets
  // under "E".
  const stripped = (ignoreArticles ? stripArticle(name) : name)
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
  const { ignoreArticles } = useSortIgnoreArticles();
  const letterToIndex = useMemo(() => {
    const map = new Map<string, number>();
    for (let i = 0; i < items.length; i++) {
      const key = firstLetterKey(items[i].name, ignoreArticles);
      if (!map.has(key)) map.set(key, i);
    }
    return map;
  }, [items, ignoreArticles]);

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
