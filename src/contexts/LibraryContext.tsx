import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { listen } from "@tauri-apps/api/event";
import { LibraryContext } from "../hooks/useLibrary";
import { useProfile } from "../hooks/useProfile";
import {
  addFolderToLibrary,
  createLibrary as apiCreateLibrary,
  deleteLibrary as apiDeleteLibrary,
  listLibraries,
  rescanLibrary as apiRescanLibrary,
  scanFolder,
  updateLibrary as apiUpdateLibrary,
  type CreateLibraryInput,
  type Library,
  type UpdateLibraryInput,
} from "../lib/tauri/library";

export function LibraryProvider({ children }: { children: ReactNode }) {
  const { activeProfile } = useProfile();
  const [libraries, setLibraries] = useState<Library[]>([]);
  const [selectedLibraryId, setSelectedLibraryId] = useState<number | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!activeProfile) {
      setLibraries([]);
      setSelectedLibraryId(null);
      return;
    }
    try {
      const list = await listLibraries();
      setLibraries(list);
      setError(null);
      // Keep the current selection if it still exists, otherwise fall back to
      // the most-recently-updated library (which is the first one because of
      // the ORDER BY in `list_libraries`).
      setSelectedLibraryId((prev) => {
        if (prev != null && list.some((l) => l.id === prev)) return prev;
        return list[0]?.id ?? null;
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      console.error("[LibraryContext] refresh failed", err);
    }
  }, [activeProfile]);

  // Re-fetch whenever the active profile changes — libraries are scoped to
  // `data.db` which is swapped on profile switch.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        if (!activeProfile) {
          if (!cancelled) {
            setLibraries([]);
            setSelectedLibraryId(null);
          }
          return;
        }
        const list = await listLibraries();
        if (cancelled) return;
        setLibraries(list);
        setSelectedLibraryId((prev) => {
          if (prev != null && list.some((l) => l.id === prev)) return prev;
          return list[0]?.id ?? null;
        });
        setError(null);
      } catch (err) {
        if (cancelled) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        console.error("[LibraryContext] initial load failed", err);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [activeProfile]);

  // The filesystem watcher emits `library:rescanned` after each
  // debounced rescan completes. Refreshing here bumps each library's
  // `updated_at`, which propagates through the `librariesSignature`
  // memo in views and re-fetches their visible track / album lists
  // without manual reloads.
  useEffect(() => {
    if (!activeProfile) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    (async () => {
      try {
        const off = await listen("library:rescanned", () => {
          refresh().catch(() => {});
        });
        if (cancelled) {
          off();
        } else {
          unlisten = off;
        }
      } catch (err) {
        console.error("[LibraryContext] listen library:rescanned failed", err);
      }
    })();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [activeProfile, refresh]);

  const selectLibrary = useCallback((libraryId: number | null) => {
    setSelectedLibraryId(libraryId);
  }, []);

  const createLibrary = useCallback(
    async (input: CreateLibraryInput) => {
      const created = await apiCreateLibrary(input);
      await refresh();
      setSelectedLibraryId(created.id);
      return created;
    },
    [refresh]
  );

  const importFolder = useCallback(
    async (libraryId: number, path: string) => {
      const folderId = await addFolderToLibrary(libraryId, path);
      const summary = await scanFolder(folderId);
      await refresh();
      return summary;
    },
    [refresh]
  );

  const updateLibrary = useCallback(
    async (libraryId: number, input: UpdateLibraryInput) => {
      await apiUpdateLibrary(libraryId, input);
      await refresh();
    },
    [refresh]
  );

  const deleteLibrary = useCallback(
    async (libraryId: number) => {
      await apiDeleteLibrary(libraryId);
      // `refresh` will pick a new selection from the remaining libraries
      // (most recently updated first) because the previous id no longer
      // matches anything in the list.
      await refresh();
    },
    [refresh]
  );

  const rescanLibrary = useCallback(
    async (libraryId: number) => {
      const summary = await apiRescanLibrary(libraryId);
      await refresh();
      return summary;
    },
    [refresh]
  );

  const selectedLibrary = useMemo(
    () =>
      selectedLibraryId == null
        ? null
        : libraries.find((l) => l.id === selectedLibraryId) ?? null,
    [libraries, selectedLibraryId]
  );

  return (
    <LibraryContext.Provider
      value={{
        libraries,
        selectedLibraryId,
        selectedLibrary,
        isLoading,
        error,
        refresh,
        selectLibrary,
        createLibrary,
        updateLibrary,
        deleteLibrary,
        rescanLibrary,
        importFolder,
      }}
    >
      {children}
    </LibraryContext.Provider>
  );
}
