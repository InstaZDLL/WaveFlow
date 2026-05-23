import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ProfileContext } from "../hooks/useProfile";
import {
  createProfile as apiCreateProfile,
  deleteProfile as apiDeleteProfile,
  getActiveProfile,
  listProfiles,
  renameProfile as apiRenameProfile,
  switchProfile as apiSwitchProfile,
  type CreateProfileInput,
  type Profile,
} from "../lib/tauri/profile";

export function ProfileProvider({ children }: { children: ReactNode }) {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [activeProfile, setActiveProfile] = useState<Profile | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [list, active] = await Promise.all([
        listProfiles(),
        getActiveProfile(),
      ]);
      setProfiles(list);
      setActiveProfile(active);
      setError(null);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setError(message);
      console.error("[ProfileContext] failed to load profiles", err);
    }
  }, []);

  // Initial load: fires once on mount, after the backend has finished
  // bootstrapping (which guarantees at least one profile is active).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      setIsLoading(true);
      try {
        const [list, active] = await Promise.all([
          listProfiles(),
          getActiveProfile(),
        ]);
        if (cancelled) return;
        setProfiles(list);
        setActiveProfile(active);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        console.error("[ProfileContext] initial load failed", err);
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const switchProfile = useCallback(
    async (profileId: number) => {
      try {
        const updated = await apiSwitchProfile(profileId);
        setActiveProfile(updated);
        // `last_used_at` changed, so the ordering of `profiles` may differ.
        await refresh();
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        throw err;
      }
    },
    [refresh],
  );

  const createProfile = useCallback(
    async (input: CreateProfileInput) => {
      try {
        const created = await apiCreateProfile(input);
        await refresh();
        return created;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        throw err;
      }
    },
    [refresh],
  );

  const deleteProfile = useCallback(
    async (profileId: number) => {
      try {
        await apiDeleteProfile(profileId);
        await refresh();
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        throw err;
      }
    },
    [refresh],
  );

  const renameProfile = useCallback(
    async (profileId: number, name: string) => {
      try {
        const updated = await apiRenameProfile(profileId, name);
        // Optimistic update so the sidebar/header reflect the new name
        // before the next list fetch resolves.
        setActiveProfile((current) =>
          current && current.id === profileId ? updated : current,
        );
        await refresh();
        return updated;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        throw err;
      }
    },
    [refresh],
  );

  return (
    <ProfileContext.Provider
      value={{
        profiles,
        activeProfile,
        isLoading,
        error,
        refresh,
        switchProfile,
        createProfile,
        deleteProfile,
        renameProfile,
      }}
    >
      {children}
    </ProfileContext.Provider>
  );
}
