import { createContext, useContext } from "react";
import type { CreateProfileInput, Profile } from "../lib/tauri/profile";

interface ProfileContextValue {
  /** List of all profiles registered in `app.db`, most-recently-used first. */
  profiles: Profile[];
  /** The profile whose `data.db` is currently opened, if any. */
  activeProfile: Profile | null;
  /** `true` until the initial fetch from the backend completes. */
  isLoading: boolean;
  /** Non-fatal error from the last backend call, if any. */
  error: string | null;
  /** Re-fetch both the list and the active profile. */
  refresh: () => Promise<void>;
  /** Switch to a different profile. Updates the active profile on success. */
  switchProfile: (profileId: number) => Promise<void>;
  /** Create a new profile. Does NOT activate it. */
  createProfile: (input: CreateProfileInput) => Promise<Profile>;
}

export const ProfileContext = createContext<ProfileContextValue | null>(null);

export function useProfile() {
  const context = useContext(ProfileContext);
  if (!context)
    throw new Error("useProfile must be used within ProfileProvider");
  return context;
}
