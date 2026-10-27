import { type GenreRow } from "../../../lib/tauri/browse";
import { type Translator } from "./libraryTypes";
import { Artwork } from "../../common/Artwork";
import { Tags } from "lucide-react";

interface GenreListProps {
  genres: GenreRow[];
  isLoading: boolean;
  t: Translator;
  onSelect: (genreId: number) => void;
}

export function GenreList({ genres, isLoading, t, onSelect }: GenreListProps) {
  return (
    <div
      className={`grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-4 ${
        isLoading ? "opacity-50" : ""
      }`}
    >
      {genres.map((genre) => (
        <button
          type="button"
          key={genre.id}
          onClick={() => onSelect(genre.id)}
          className="flex items-center space-x-3 p-4 rounded-2xl border border-zinc-200 bg-white hover:bg-zinc-50 dark:border-zinc-800 dark:bg-zinc-800/40 dark:hover:bg-zinc-800/70 transition-colors cursor-pointer text-left focus:outline-none focus:ring-2 focus:ring-emerald-500/40"
        >
          <Artwork
            path={genre.artwork_path}
            path1x={genre.artwork_path_1x}
            path2x={genre.artwork_path_2x}
            size="1x"
            className="w-12 h-12"
            rounded="xl"
            iconSize={22}
            placeholderIcon={Tags}
            alt=""
          />
          <div className="flex-1 min-w-0">
            <div className="text-sm font-semibold text-zinc-800 dark:text-zinc-200 truncate">
              {genre.name}
            </div>
            <div className="text-xs text-zinc-500">
              {t("library.genreList.trackCount", { count: genre.track_count })}
            </div>
          </div>
        </button>
      ))}
    </div>
  );
}
