import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Search, User, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import { AnimatedModalContent, AnimatedModalShell } from "./AnimatedModalShell";
import {
  getArtistOverrides,
  setArtistMetadataOverrides,
  type ArtistOverrideSimilar,
} from "../../lib/tauri/artistOverrides";
import { searchArtists, type ArtistRow } from "../../lib/tauri/browse";
import { resolveRemoteImage } from "../../lib/tauri/artwork";

/** Mirror of the backend `MAX_SIMILAR` cap so the UI blocks before the
 *  server would silently truncate. */
const MAX_SIMILAR = 50;

interface ArtistMetadataEditorModalProps {
  artistId: number;
  artistName: string;
  isOpen: boolean;
  onClose: () => void;
  /** Fired after a successful save so the detail view re-reads the
   *  (now overridden) bio + similar list. */
  onSuccess: () => void;
}

/**
 * Per-artist override editor (issue #323). Two offline-first controls:
 *  - a free-text bio that replaces the fetched Last.fm/TheAudioDB one
 *  - a library-scoped, user-curated similar-artist list
 *
 * Both persist per-profile and survive enrichment passes. Clearing the
 * bio field or removing every chip drops the respective override so the
 * online value takes back over.
 */
export function ArtistMetadataEditorModal({
  artistId,
  artistName,
  isOpen,
  onClose,
  onSuccess,
}: ArtistMetadataEditorModalProps) {
  const { t } = useTranslation();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);

  const [bio, setBio] = useState("");
  const [selected, setSelected] = useState<ArtistOverrideSimilar[]>([]);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<ArtistRow[]>([]);
  const [isSearching, setIsSearching] = useState(false);
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const debounceRef = useRef<number | null>(null);
  // Monotonic counter so a slow earlier response can't overwrite a
  // newer query's results.
  const requestIdRef = useRef(0);

  // Prefill from the current override state every time the modal opens.
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    // Clear EVERYTHING up-front — including bio + chips — so a different
    // artist never flashes the previous one's data while the async read
    // is in flight.
    /* eslint-disable react-hooks/set-state-in-effect */
    setIsLoading(true);
    setError(null);
    setQuery("");
    setResults([]);
    setBio("");
    setSelected([]);
    /* eslint-enable react-hooks/set-state-in-effect */
    getArtistOverrides(artistId)
      .then((ov) => {
        if (cancelled) return;
        setBio(ov.custom_bio ?? "");
        setSelected(ov.similar);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error("[ArtistMetadataEditorModal] load failed", err);
        setError(String(err));
      })
      .finally(() => {
        if (!cancelled) setIsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [isOpen, artistId]);

  const selectedIds = useMemo(
    () => new Set(selected.map((s) => s.artist_id)),
    [selected],
  );

  // Single source of truth for "don't touch state right now": the
  // initial read is in flight (would clobber local edits) or a save is
  // running (would submit stale state).
  const isBusy = isLoading || isSaving;

  // Debounced library search for the similar-artist autocomplete.
  useEffect(() => {
    if (!isOpen || isBusy) return;
    if (debounceRef.current != null) window.clearTimeout(debounceRef.current);
    // Invalidate any in-flight request the instant the query changes —
    // bumping here (not inside the timeout) means a response that lands
    // during the debounce window fails the `requestId` check and can't
    // apply stale hits.
    const requestId = ++requestIdRef.current;
    const trimmed = query.trim();
    if (trimmed.length < 2) {
      /* eslint-disable react-hooks/set-state-in-effect */
      setResults([]);
      setIsSearching(false);
      /* eslint-enable react-hooks/set-state-in-effect */
      return;
    }
    debounceRef.current = window.setTimeout(() => {
      setIsSearching(true);
      // Drop the previous query's hits immediately so stale entries
      // aren't shown (or clickable) while the new request is in flight.
      setResults([]);
      searchArtists(trimmed, null, 8)
        .then((res) => {
          if (requestId !== requestIdRef.current) return;
          // Drop the artist itself + anyone already picked.
          setResults(
            res.filter(
              (r) => r.id !== artistId && !selectedIds.has(r.id),
            ),
          );
        })
        .catch((err) => {
          if (requestId !== requestIdRef.current) return;
          console.error("[ArtistMetadataEditorModal] search failed", err);
          setResults([]);
        })
        .finally(() => {
          if (requestId !== requestIdRef.current) return;
          setIsSearching(false);
        });
    }, 250);
    return () => {
      if (debounceRef.current != null) window.clearTimeout(debounceRef.current);
    };
  }, [query, isOpen, isBusy, artistId, selectedIds]);

  const atLimit = selected.length >= MAX_SIMILAR;

  const addArtist = (row: ArtistRow) => {
    if (isBusy) return;
    // Enforce the cap + dedup inside the updater so rapid clicks
    // validate against the latest `prev`, not a render-stale snapshot.
    setSelected((prev) => {
      if (
        prev.length >= MAX_SIMILAR ||
        prev.some((s) => s.artist_id === row.id)
      ) {
        return prev;
      }
      return [
        ...prev,
        {
          artist_id: row.id,
          name: row.name,
          picture_url: row.picture_url,
          picture_path: row.picture_path,
        },
      ];
    });
    setQuery("");
    setResults([]);
  };

  const removeArtist = (id: number) => {
    if (isBusy) return;
    setSelected((prev) => prev.filter((s) => s.artist_id !== id));
  };

  const handleSave = async () => {
    if (isSaving) return;
    setIsSaving(true);
    setError(null);
    try {
      const trimmedBio = bio.trim();
      // One transactional write so bio + similar can't half-apply.
      await setArtistMetadataOverrides(
        artistId,
        trimmedBio.length ? trimmedBio : null,
        selected.length ? selected.map((s) => s.artist_id) : null,
      );
      onSuccess();
      onClose();
    } catch (err) {
      console.error("[ArtistMetadataEditorModal] save failed", err);
      setError(String(err));
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="artist-metadata-editor-title"
        className="relative w-full max-w-2xl rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated max-h-[90vh] overflow-hidden flex flex-col"
      >
        <h2
          id="artist-metadata-editor-title"
          className="text-lg font-bold text-zinc-900 dark:text-white mb-1"
        >
          {t("artistMetadataEditor.title")}
        </h2>
        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4">
          {t("artistMetadataEditor.subtitle", { name: artistName })}
        </p>

        {error && <div className="mb-3 text-xs text-red-500 px-1">{error}</div>}

        <div className="flex-1 overflow-y-auto space-y-6 pr-1">
          {/* Bio override */}
          <section className="space-y-2">
            <div className="flex items-center justify-between">
              <label
                htmlFor="artist-bio-override"
                className="text-sm font-semibold text-zinc-800 dark:text-zinc-200"
              >
                {t("artistMetadataEditor.bio.label")}
              </label>
              {bio.trim().length > 0 && !isBusy && (
                <button
                  type="button"
                  onClick={() => setBio("")}
                  className="text-xs font-medium text-emerald-600 dark:text-emerald-400 hover:underline"
                >
                  {t("artistMetadataEditor.bio.reset")}
                </button>
              )}
            </div>
            <textarea
              id="artist-bio-override"
              value={bio}
              onChange={(e) => setBio(e.target.value)}
              rows={5}
              disabled={isBusy}
              placeholder={t("artistMetadataEditor.bio.placeholder")}
              className="w-full px-3 py-2.5 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-sm text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent resize-y disabled:opacity-50"
            />
            <p className="text-xs text-zinc-400">
              {t("artistMetadataEditor.bio.help")}
            </p>
          </section>

          {/* Similar override */}
          <section className="space-y-2">
            <label
              htmlFor="artist-similar-search"
              className="text-sm font-semibold text-zinc-800 dark:text-zinc-200"
            >
              {t("artistMetadataEditor.similar.label")}
            </label>

            {/* Selected chips */}
            {selected.length > 0 && (
              <ul className="flex flex-wrap gap-2">
                {selected.map((s) => {
                  const img = resolveRemoteImage(s.picture_path, s.picture_url);
                  return (
                    <li key={s.artist_id}>
                      <span className="inline-flex items-center gap-2 pl-1 pr-2 py-1 rounded-full bg-zinc-100 dark:bg-zinc-800 text-sm text-zinc-800 dark:text-zinc-200">
                        {img ? (
                          <img
                            src={img}
                            alt=""
                            className="w-6 h-6 rounded-full object-cover"
                          />
                        ) : (
                          <span className="w-6 h-6 rounded-full bg-zinc-200 dark:bg-zinc-700 flex items-center justify-center text-zinc-400">
                            <User size={12} />
                          </span>
                        )}
                        <span className="truncate max-w-48">{s.name}</span>
                        <button
                          type="button"
                          onClick={() => removeArtist(s.artist_id)}
                          disabled={isBusy}
                          aria-label={t("artistMetadataEditor.similar.remove", {
                            name: s.name,
                          })}
                          className="text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 disabled:opacity-50"
                        >
                          <X size={14} />
                        </button>
                      </span>
                    </li>
                  );
                })}
              </ul>
            )}

            {/* Autocomplete input */}
            <div className="relative">
              <Search
                size={16}
                className="absolute left-3 top-1/2 -translate-y-1/2 text-zinc-400"
              />
              <input
                id="artist-similar-search"
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                disabled={isBusy || atLimit}
                placeholder={t("artistMetadataEditor.similar.placeholder")}
                className="w-full pl-9 pr-9 py-2.5 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-sm text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent disabled:opacity-50"
              />
              {isSearching && (
                <Loader2
                  size={16}
                  className="absolute right-3 top-1/2 -translate-y-1/2 text-zinc-400 animate-spin"
                />
              )}
            </div>

            {atLimit && (
              <p className="text-xs text-amber-600 dark:text-amber-400">
                {t("artistMetadataEditor.similar.limitReached", {
                  max: MAX_SIMILAR,
                })}
              </p>
            )}

            {/* Results — plain list of action buttons (not a listbox: no
                roving focus / active-option semantics implemented). */}
            {!isBusy && !atLimit && results.length > 0 && (
              <ul className="max-h-56 overflow-y-auto rounded-xl border border-zinc-200 dark:border-zinc-700 divide-y divide-zinc-100 dark:divide-zinc-800">
                {results.map((r) => {
                  const img = resolveRemoteImage(r.picture_path, r.picture_url);
                  return (
                    <li key={r.id}>
                      <button
                        type="button"
                        onClick={() => addArtist(r)}
                        className="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-zinc-50 dark:hover:bg-zinc-800/40 transition-colors"
                      >
                        {img ? (
                          <img
                            src={img}
                            alt=""
                            className="w-8 h-8 rounded-full object-cover shrink-0"
                          />
                        ) : (
                          <span className="w-8 h-8 rounded-full bg-zinc-100 dark:bg-zinc-800 flex items-center justify-center text-zinc-400 shrink-0">
                            <User size={14} />
                          </span>
                        )}
                        <span className="min-w-0">
                          <span className="block text-sm text-zinc-800 dark:text-zinc-200 truncate">
                            {r.name}
                          </span>
                          <span className="block text-xs text-zinc-400">
                            {t("artistDetail.trackCount", {
                              count: r.track_count,
                            })}
                          </span>
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
            <p className="text-xs text-zinc-400">
              {t("artistMetadataEditor.similar.help")}
            </p>
          </section>
        </div>

        {/* Footer */}
        <div className="mt-4 flex items-center justify-end gap-2 pt-3 border-t border-zinc-100 dark:border-zinc-800">
          <button
            type="button"
            onClick={onClose}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={isSaving || isLoading}
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-5 py-2 rounded-xl text-sm font-semibold flex items-center gap-2 transition-colors shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {isSaving && <Loader2 size={14} className="animate-spin" />}
            <span>{t("common.save")}</span>
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
