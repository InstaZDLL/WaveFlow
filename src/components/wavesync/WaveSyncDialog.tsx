import { useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, RefreshCw, Users, Radio as RadioIcon } from "lucide-react";

import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../common/AnimatedModalShell";
import { useModalA11y } from "../../hooks/useModalA11y";
import { useProfile } from "../../hooks/useProfile";
import { usePlayer } from "../../hooks/usePlayer";
import { closeWaveSyncDialog, useWaveSync } from "../../hooks/useWaveSync";
import { backendErrorText } from "../../lib/backendError";
import {
  wavesyncDiscover,
  wavesyncJoin,
  wavesyncStart,
  type WaveSyncDiscovered,
} from "../../lib/tauri/wavesync";

const FIELD =
  "w-full px-4 py-2.5 rounded-xl bg-zinc-50 dark:bg-zinc-800/50 border border-zinc-200 dark:border-zinc-700 text-zinc-900 dark:text-white placeholder:text-zinc-400 dark:placeholder:text-zinc-500 focus:outline-none focus:ring-2 focus:ring-emerald-500 focus:border-transparent transition-colors";
const LABEL =
  "block text-[10px] font-bold tracking-widest text-zinc-500 uppercase mb-2";

type Tab = "start" | "join";

/**
 * Start a WaveSync session or join one (RFC-010). Opened from the player
 * bar's "⋯" menu while no session runs; once one does, the menu opens the
 * WaveSync panel instead.
 */
export function WaveSyncDialog() {
  const { t } = useTranslation();
  const { dialogOpen } = useWaveSync();
  const { activeProfile } = useProfile();
  const { openRightPanel } = usePlayer();
  const dialogRef = useModalA11y<HTMLDivElement>(
    dialogOpen,
    closeWaveSyncDialog,
  );
  const baseId = useId();

  const [tab, setTab] = useState<Tab>("start");
  const [displayName, setDisplayName] = useState("");
  const [sessionName, setSessionName] = useState("");
  const [approval, setApproval] = useState(true);
  const [target, setTarget] = useState("");
  const [picked, setPicked] = useState<WaveSyncDiscovered | null>(null);
  const [code, setCode] = useState("");
  const [found, setFound] = useState<WaveSyncDiscovered[]>([]);
  const [looking, setLooking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const profileName = activeProfile?.name ?? "";

  // Fresh fields each time the dialog opens.
  useEffect(() => {
    if (!dialogOpen) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reset on open
    setDisplayName(profileName);
    setSessionName(
      profileName ? t("wavesync.start.defaultName", { name: profileName }) : "",
    );
    setError(null);
    setCode("");
    setTarget("");
    setPicked(null);
  }, [dialogOpen, profileName, t]);

  const lookAround = useCallback(async () => {
    setLooking(true);
    setError(null);
    try {
      setFound(await wavesyncDiscover());
    } catch (err) {
      setError(backendErrorText(err, t));
    } finally {
      setLooking(false);
    }
  }, [t]);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- discovery runs when the tab shows
    if (dialogOpen && tab === "join") void lookAround();
  }, [dialogOpen, tab, lookAround]);

  const done = () => {
    closeWaveSyncDialog();
    openRightPanel("wavesync");
  };

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      await wavesyncStart(sessionName, displayName, approval);
      done();
    } catch (err) {
      setError(backendErrorText(err, t));
    } finally {
      setBusy(false);
    }
  };

  const join = async () => {
    setBusy(true);
    setError(null);
    try {
      await wavesyncJoin(
        picked ? picked.address : target,
        picked?.fingerprint ?? null,
        code || null,
        displayName,
      );
      done();
    } catch (err) {
      setError(backendErrorText(err, t));
    } finally {
      setBusy(false);
    }
  };

  const canStart =
    !busy && displayName.trim() !== "" && sessionName.trim() !== "";
  const typedInvitation = target.trim().startsWith("wavesync://");
  const canJoin =
    !busy &&
    displayName.trim() !== "" &&
    (picked != null || target.trim() !== "") &&
    (code.trim() !== "" || (picked == null && typedInvitation));

  return (
    <AnimatedModalShell
      isOpen={dialogOpen}
      onBackdropClick={closeWaveSyncDialog}
    >
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={`${baseId}-title`}
        className="relative w-full max-w-md rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated"
      >
        <h2
          id={`${baseId}-title`}
          className="text-lg font-bold text-zinc-900 dark:text-white"
        >
          WaveSync
        </h2>
        <p className="mt-1 mb-4 text-sm text-zinc-500">
          {t("wavesync.dialog.intro")}
        </p>

        <div
          role="tablist"
          aria-label={t("wavesync.dialog.choose")}
          className="mb-5 grid grid-cols-2 gap-1 rounded-xl bg-zinc-100 p-1 dark:bg-zinc-800/60"
        >
          {(["start", "join"] as Tab[]).map((id) => (
            <button
              key={id}
              type="button"
              role="tab"
              aria-selected={tab === id}
              onClick={() => {
                setTab(id);
                setError(null);
              }}
              className={`flex items-center justify-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors ${
                tab === id
                  ? "bg-white text-zinc-900 shadow-sm dark:bg-zinc-900 dark:text-white"
                  : "text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
              }`}
            >
              {id === "start" ? <RadioIcon size={14} /> : <Users size={14} />}
              {t(
                id === "start"
                  ? "wavesync.dialog.startTab"
                  : "wavesync.dialog.joinTab",
              )}
            </button>
          ))}
        </div>

        <div className="mb-4">
          <label htmlFor={`${baseId}-me`} className={LABEL}>
            {t("wavesync.dialog.yourName")}
          </label>
          <input
            id={`${baseId}-me`}
            type="text"
            value={displayName}
            maxLength={40}
            onChange={(e) => setDisplayName(e.target.value)}
            className={FIELD}
          />
        </div>

        {tab === "start" ? (
          <>
            <div className="mb-4">
              <label htmlFor={`${baseId}-session`} className={LABEL}>
                {t("wavesync.dialog.sessionName")}
              </label>
              <input
                id={`${baseId}-session`}
                type="text"
                value={sessionName}
                maxLength={40}
                onChange={(e) => setSessionName(e.target.value)}
                className={FIELD}
              />
            </div>
            <label className="mb-3 flex items-start gap-3 text-sm text-zinc-700 dark:text-zinc-200">
              <input
                type="checkbox"
                checked={approval}
                onChange={(e) => setApproval(e.target.checked)}
                className="mt-0.5 h-4 w-4 accent-emerald-500"
              />
              <span>{t("wavesync.dialog.approval")}</span>
            </label>
            <p className="mb-5 text-xs text-zinc-500">
              {t("wavesync.dialog.firewall")}
            </p>
          </>
        ) : (
          <>
            <div className="mb-4">
              <div className="mb-2 flex items-center justify-between">
                <span className={`${LABEL} mb-0`}>
                  {t("wavesync.dialog.nearby")}
                </span>
                <button
                  type="button"
                  onClick={() => void lookAround()}
                  disabled={looking}
                  aria-label={t("wavesync.dialog.lookAgain")}
                  title={t("wavesync.dialog.lookAgain")}
                  className="rounded-full p-1.5 text-zinc-500 hover:bg-zinc-100 disabled:opacity-50 dark:hover:bg-zinc-800"
                >
                  {looking ? (
                    <Loader2 size={14} className="animate-spin" />
                  ) : (
                    <RefreshCw size={14} />
                  )}
                </button>
              </div>
              {found.length === 0 ? (
                <p className="rounded-xl bg-zinc-50 px-3 py-2.5 text-sm text-zinc-500 dark:bg-zinc-800/40">
                  {looking
                    ? t("wavesync.dialog.looking")
                    : t("wavesync.dialog.noneNearby")}
                </p>
              ) : (
                <ul
                  className="space-y-1.5"
                  role="listbox"
                  aria-label={t("wavesync.dialog.nearby")}
                >
                  {found.map((session) => {
                    const selected = picked?.address === session.address;
                    return (
                      <li key={session.address}>
                        <button
                          type="button"
                          role="option"
                          aria-selected={selected}
                          onClick={() => setPicked(selected ? null : session)}
                          className={`w-full rounded-xl border px-3 py-2 text-start text-sm transition-colors ${
                            selected
                              ? "border-emerald-500 bg-emerald-50 dark:bg-emerald-900/20"
                              : "border-zinc-200 hover:bg-zinc-50 dark:border-zinc-700 dark:hover:bg-zinc-800/50"
                          }`}
                        >
                          <span className="block truncate font-medium text-zinc-900 dark:text-white">
                            {session.name || session.address}
                          </span>
                          <span className="block truncate text-xs text-zinc-500">
                            {t("wavesync.dialog.hostedBy", {
                              name: session.host,
                            })}
                          </span>
                        </button>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
            {picked == null && (
              <div className="mb-4">
                <label htmlFor={`${baseId}-target`} className={LABEL}>
                  {t("wavesync.dialog.address")}
                </label>
                <input
                  id={`${baseId}-target`}
                  type="text"
                  dir="ltr"
                  value={target}
                  onChange={(e) => setTarget(e.target.value)}
                  placeholder="192.168.1.20:43399"
                  className={FIELD}
                />
              </div>
            )}
            <div className="mb-5">
              <label htmlFor={`${baseId}-code`} className={LABEL}>
                {t("wavesync.dialog.code")}
              </label>
              <input
                id={`${baseId}-code`}
                type="text"
                dir="ltr"
                value={code}
                autoComplete="off"
                spellCheck={false}
                onChange={(e) => setCode(e.target.value)}
                placeholder="ABCD-EFGH"
                className={`${FIELD} font-mono uppercase tracking-widest`}
              />
              <p className="mt-2 text-xs text-zinc-500">
                {t("wavesync.dialog.codeHint")}
              </p>
            </div>
          </>
        )}

        {error && (
          <p
            role="alert"
            className="mb-4 rounded-xl bg-red-50 px-3 py-2 text-sm text-red-700 dark:bg-red-900/20 dark:text-red-300"
          >
            {error}
          </p>
        )}

        <div className="flex items-center justify-end gap-3">
          <button
            type="button"
            onClick={closeWaveSyncDialog}
            className="rounded-xl px-4 py-2 text-sm font-medium text-zinc-500 transition-colors hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={() => void (tab === "start" ? start() : join())}
            disabled={tab === "start" ? !canStart : !canJoin}
            className="flex items-center gap-2 rounded-xl bg-emerald-500 px-5 py-2 text-sm font-semibold text-white shadow-lg shadow-emerald-500/20 transition-all hover:bg-emerald-400 active:scale-[0.98] disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy && <Loader2 size={16} className="animate-spin" />}
            {t(
              tab === "start"
                ? "wavesync.dialog.start"
                : "wavesync.dialog.join",
            )}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
