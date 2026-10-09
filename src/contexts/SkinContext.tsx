import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { SkinContext } from "../hooks/useSkin";
import { useProfile } from "../hooks/useProfile";
import {
  applySkin,
  DEFAULT_SKIN_ID,
  findSkin,
  SKIN_PRESETS,
  type SkinPreset,
} from "../lib/skins";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { completeSkinRecovery, recordSkin } from "../lib/tauri/skinRecovery";

const PROFILE_SETTING_KEY = "appearance.skin.id";
// First-paint cache. Source of truth lives in the active profile's
// `profile_setting`; see [`ThemeContext`](./ThemeContext.tsx) for the
// same hybrid storage rationale.
const SKIN_CACHE_KEY = "waveflow.skin.id";
const SAFE_MODE =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("safe-mode") === "1";

function clearSafeModeQuery() {
  if (!SAFE_MODE) return;
  const url = new URL(window.location.href);
  url.searchParams.delete("safe-mode");
  window.history.replaceState(window.history.state, "", url);
}

const readCachedSkin = (): SkinPreset => {
  if (SAFE_MODE) return findSkin(DEFAULT_SKIN_ID);
  if (typeof window === "undefined") return findSkin(DEFAULT_SKIN_ID);
  try {
    const stored = window.localStorage.getItem(SKIN_CACHE_KEY);
    return findSkin(stored);
  } catch {
    return findSkin(DEFAULT_SKIN_ID);
  }
};

const writeCachedSkin = (id: string) => {
  if (typeof window === "undefined") return false;
  try {
    window.localStorage.setItem(SKIN_CACHE_KEY, id);
    return true;
  } catch {
    // localStorage unavailable — DB value still wins on next launch.
    return false;
  }
};

/**
 * Skin provider. Mirrors [`ThemeProvider`](./ThemeContext.tsx)'s
 * hybrid storage (DB source-of-truth + localStorage first-paint
 * cache) without the View Transitions API integration — a skin swap
 * is a subtler change than a theme swap and a plain re-render is
 * enough. Iterating on a richer cross-fade can land in a follow-up
 * if user feedback asks for it.
 */
export function SkinProvider({ children }: { children: ReactNode }) {
  const [skin, setSkin] = useState<SkinPreset>(readCachedSkin);
  const [skinError, setSkinError] = useState(false);
  const selectionQueue = useRef<Promise<void>>(Promise.resolve());
  const selectionVersion = useRef(0);
  const recoveryProfileId = useRef<number | null>(null);
  const recoverySaved = useRef(false);
  const recoveryNeedsCompletion = useRef(false);
  const recoveryCompletion = useRef<Promise<void> | null>(null);
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  const activeProfileIdRef = useRef(activeProfileId);

  useEffect(() => {
    activeProfileIdRef.current = activeProfileId;
  }, [activeProfileId]);

  useEffect(() => {
    applySkin(skin);
  }, [skin]);

  // Source-of-truth read on mount + profile switch. Same pattern as
  // `ThemeProvider` — the cache may hold the previous profile's
  // choice, the DB row wins.
  useEffect(() => {
    if (!activeProfile) return;
    let cancelled = false;
    // Captured before any await so both calls stay pinned to the profile
    // this effect ran for — `cancelled` is checked before the seeding
    // write, not during its IPC hop (issue #485).
    const profileId = activeProfile.id;
    const version = selectionVersion.current;
    if (SAFE_MODE && recoveryProfileId.current === null) {
      recoveryProfileId.current = profileId;
    }
    (async () => {
      try {
        if (
          SAFE_MODE &&
          !recoverySaved.current &&
          recoveryProfileId.current === profileId
        ) {
          // The URL already forced Studio before the first paint. Persist it
          // in the active profile as well, otherwise the next launch would
          // load the unusable skin again from the database.
          if (!recoveryCompletion.current) {
            recoveryCompletion.current = (async () => {
              await setProfileSetting(
                PROFILE_SETTING_KEY,
                DEFAULT_SKIN_ID,
                "string",
                profileId,
              );
              if (!writeCachedSkin(DEFAULT_SKIN_ID)) {
                throw new Error("Could not update the first-paint skin cache");
              }
              setSkin(findSkin(DEFAULT_SKIN_ID));
              // Only the first active profile is reset. The shared promise
              // survives React StrictMode's effect replay.
              recoverySaved.current = true;
              await completeSkinRecovery();
              recoveryNeedsCompletion.current = false;
              clearSafeModeQuery();
            })();
          }
          try {
            await recoveryCompletion.current;
            if (!cancelled) setSkinError(false);
          } catch (err) {
            recoveryNeedsCompletion.current = recoverySaved.current;
            if (!cancelled) setSkinError(true);
            console.warn("[SkinContext] Studio recovery failed", err);
          }
          return;
        }
        if (SAFE_MODE && recoveryCompletion.current) {
          try {
            await recoveryCompletion.current;
          } catch (err) {
            if (!recoverySaved.current) throw err;
            await completeSkinRecovery();
            recoveryNeedsCompletion.current = false;
            recoveryCompletion.current = Promise.resolve();
            clearSafeModeQuery();
          }
          if (cancelled || version !== selectionVersion.current) return;
        }
        const stored = await getProfileSetting(PROFILE_SETTING_KEY, profileId);
        if (cancelled || version !== selectionVersion.current) return;
        if (stored) {
          const fromDb = findSkin(stored);
          await recordSkin(fromDb.id);
          if (cancelled || version !== selectionVersion.current) return;
          if (fromDb.id !== skin.id) {
            setSkin(fromDb);
            writeCachedSkin(fromDb.id);
          }
          return;
        }
        // First-time seed from the currently applied skin so future
        // reads have something to anchor on.
        await recordSkin(skin.id);
        if (cancelled || version !== selectionVersion.current) return;
        await setProfileSetting(
          PROFILE_SETTING_KEY,
          skin.id,
          "string",
          profileId,
        );
      } catch (err) {
        if (
          !cancelled &&
          version === selectionVersion.current &&
          profileId === activeProfileIdRef.current
        ) {
          setSkinError(true);
        }
        console.warn("[SkinContext] profile-scoped skin load failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProfile?.id]);

  const setSkinId = useCallback((id: string) => {
    const next = findSkin(id);
    const profileId = activeProfileIdRef.current;
    const version = ++selectionVersion.current;
    // Serialize rapid picker changes so an older IPC call cannot leave the
    // recovery marker pointing at a skin different from the one on screen.
    selectionQueue.current = selectionQueue.current
      .catch(() => undefined)
      .then(async () => {
        const stillCurrent = () =>
          version === selectionVersion.current &&
          profileId === activeProfileIdRef.current;
        if (!stillCurrent()) return;
        if (recoveryCompletion.current) {
          try {
            await recoveryCompletion.current;
          } catch (err) {
            if (!recoverySaved.current) throw err;
            recoveryNeedsCompletion.current = true;
          }
        }
        if (recoveryNeedsCompletion.current) {
          await completeSkinRecovery();
          recoveryNeedsCompletion.current = false;
          recoveryCompletion.current = Promise.resolve();
          clearSafeModeQuery();
        }
        if (SAFE_MODE && !recoverySaved.current) {
          throw new Error("Studio recovery has not saved the active profile");
        }
        if (!stillCurrent()) return;
        if (next.id !== DEFAULT_SKIN_ID) await recordSkin(next.id);
        if (!stillCurrent()) return;
        await setProfileSetting(
          PROFILE_SETTING_KEY,
          next.id,
          "string",
          profileId,
        );
        if (!stillCurrent()) return;
        if (next.id === DEFAULT_SKIN_ID) await recordSkin(next.id);
        if (!stillCurrent()) return;
        writeCachedSkin(next.id);
        setSkin(next);
        setSkinError(false);
      })
      .catch((err) => {
        setSkinError(true);
        console.warn("[SkinContext] skin change failed", err);
      });
  }, []);

  return (
    <SkinContext.Provider value={{ skin, setSkinId, skinError }}>
      {children}
    </SkinContext.Provider>
  );
}

export { SKIN_PRESETS };
