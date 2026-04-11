import { createContext, useContext } from "react";
import type {
  CreateLibraryInput,
  Library,
  RescanSummary,
  ScanSummary,
  UpdateLibraryInput,
} from "../lib/tauri/library";

interface LibraryContextValue {
  /** All libraries belonging to the currently active profile. */
  libraries: Library[];
  /**
   * Id of the library the user has currently "focused" in the sidebar.
   * `null` until libraries are loaded or when the profile has no libraries.
   */
  selectedLibraryId: number | null;
  selectedLibrary: Library | null;
  isLoading: boolean;
  error: string | null;
  /** Re-fetch the list (called after mutations like create / import). */
  refresh: () => Promise<void>;
  /** Switch which library the sidebar has focused. */
  selectLibrary: (libraryId: number | null) => void;
  /** Create a new library in the active profile. */
  createLibrary: (input: CreateLibraryInput) => Promise<Library>;
  /** Patch an existing library's name / description / color / icon. */
  updateLibrary: (
    libraryId: number,
    input: UpdateLibraryInput
  ) => Promise<void>;
  /** Permanently delete a library and everything it owns. */
  deleteLibrary: (libraryId: number) => Promise<void>;
  /** Re-walk every folder of the library and sync the DB with disk. */
  rescanLibrary: (libraryId: number) => Promise<RescanSummary>;
  /**
   * Register a folder inside a library and immediately scan it. Returns the
   * summary so the UI can surface counts to the user.
   */
  importFolder: (libraryId: number, path: string) => Promise<ScanSummary>;
}

export const LibraryContext = createContext<LibraryContextValue | null>(null);

export function useLibrary() {
  const context = useContext(LibraryContext);
  if (!context)
    throw new Error("useLibrary must be used within LibraryProvider");
  return context;
}
