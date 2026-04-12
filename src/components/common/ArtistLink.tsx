import type { MouseEvent } from "react";

interface ArtistLinkProps {
  /**
   * Full artist name string as returned by the backend. May contain
   * multiple artists joined by `", "` (e.g. "Bcalm, Banks, Hendy").
   */
  name: string | null | undefined;
  /**
   * Comma-joined artist IDs in the same order as `name`'s entries
   * (e.g. `"12,45,78"`). When provided, each name becomes individually
   * clickable. If shorter than the names array, trailing names are
   * rendered as plain text.
   */
  artistIds: string | null | undefined;
  onNavigate: (artistId: number) => void;
  /** Fallback text shown when `name` is null/empty (e.g. "—"). */
  fallback?: string;
  /** Optional class applied to the wrapper span. */
  className?: string;
}

/**
 * Render a multi-artist credit string where every individually-known
 * artist is wrapped in a button that navigates to its detail page.
 *
 * Input is the pair `("Lynde, Teau", "12,45")` — the component splits
 * both on their respective separators and zips them by index. Entries
 * without a matching ID render as plain text so we never navigate to
 * the wrong page.
 *
 * Clicks stop propagation so the link doesn't trigger row-level
 * handlers like double-click-to-play.
 */
export function ArtistLink({
  name,
  artistIds,
  onNavigate,
  fallback = "—",
  className = "",
}: ArtistLinkProps) {
  if (!name || !name.trim()) {
    return <span className={className}>{fallback}</span>;
  }

  const names = name.split(", ");
  const ids = (artistIds ?? "")
    .split(",")
    .map((s) => s.trim())
    .map((s) => (s === "" ? null : Number(s)))
    .map((n) => (n != null && Number.isFinite(n) ? n : null));

  const handleClick = (e: MouseEvent, id: number) => {
    e.stopPropagation();
    onNavigate(id);
  };

  return (
    <span className={className}>
      {names.map((part, index) => {
        const id = ids[index] ?? null;
        return (
          <span key={`${part}-${index}`}>
            {index > 0 && ", "}
            {id != null ? (
              <button
                type="button"
                onClick={(e) => handleClick(e, id)}
                className="hover:underline hover:text-emerald-600 dark:hover:text-emerald-400 transition-colors cursor-pointer"
              >
                {part}
              </button>
            ) : (
              <span>{part}</span>
            )}
          </span>
        );
      })}
    </span>
  );
}
