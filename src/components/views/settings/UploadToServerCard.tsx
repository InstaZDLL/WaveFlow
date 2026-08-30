import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useProfile } from "../../../hooks/useProfile";
import { listen } from "@tauri-apps/api/event";
import { Loader2, Search, Upload, X } from "lucide-react";
import {
  remoteCancelUpload,
  remoteGetStatus,
  remoteUploadLibraries,
  remoteUploadSurvey,
  remoteUploadTracks,
  type UploadLibrary,
  type UploadOutcome,
  type UploadPlan,
  type UploadProgress,
  type UploadSurveyProgress,
} from "../../../lib/tauri/remoteServer";

/**
 * Settings → sending the server what it does not have.
 *
 * The other half of the balance: the library can already pull from the server,
 * and without this the two collections drift apart by design.
 *
 * Two steps rather than one button, because they cost different things. The
 * survey reads every unlinked file to compute a whole-file digest — the price
 * of an identity the server can recognise, paid once and cached — and it is
 * also where most of the work disappears: a digest the mirrored catalogue
 * already knows is a track the server has, linked offline without a single
 * request. Only what is left is offered.
 *
 * Hides itself when `sync_v2` is absent, like every other remote surface.
 */
export function UploadToServerCard() {
  const { t } = useTranslation();
  const [visible, setVisible] = useState(false);
  const [libraries, setLibraries] = useState<UploadLibrary[]>([]);
  const [picked, setPicked] = useState<string | null>(null);
  const [plan, setPlan] = useState<UploadPlan | null>(null);
  const [outcome, setOutcome] = useState<UploadOutcome | null>(null);
  const [surveyProgress, setSurveyProgress] =
    useState<UploadSurveyProgress | null>(null);
  const [uploadProgress, setUploadProgress] = useState<UploadProgress | null>(
    null,
  );
  const [busy, setBusy] = useState<null | "survey" | "upload">(null);
  const [error, setError] = useState<string | null>(null);
  const mountedRef = useRef(false);
  const { activeProfile } = useProfile();
  const activeProfileId = activeProfile?.id ?? null;
  // Which profile a result belongs to. `mountedRef` cannot answer that: a
  // profile switch leaves this card mounted, so a survey or an upload started
  // under the previous profile resolves with the flag still true and paints
  // its result over the reset. That matters beyond a stale figure — the plan
  // carries local rowids, and rowids are per-profile, so a plan restored after
  // the switch would offer the *new* profile's tracks by the *old* one's
  // numbers.
  const generationRef = useRef(0);

  // Subscribe before the first read, for the reason the mirror card gives: an
  // event emitted while `listen()` is still resolving is lost for good.
  //
  // Keyed on the active profile, not mounted once. A profile switch does not
  // remount this tree — it swaps a value in the context — so a card that read
  // its destinations at mount would keep offering the *previous* profile's
  // server libraries, while the backend resolves the pool afresh on every
  // call. Uploading writes to somebody's server; it is the last place to act
  // on a stale picture.
  useEffect(() => {
    mountedRef.current = true;
    generationRef.current += 1;
    const generation = generationRef.current;
    // True only while this profile is still the one on screen. Every write
    // below goes through it, including the ones inside the listeners: a pass
    // still running for the outgoing profile keeps emitting, and its bar must
    // not move under the incoming one.
    const current = () => mountedRef.current && generationRef.current === generation;
    const offs: (() => void)[] = [];
    // A listener whose `listen()` resolves after the cleanup has run would
    // never be removed by it: the array it would be pushed onto has already
    // been walked. So each one is either detached immediately or registered
    // for the cleanup to find, decided after the await rather than before it.
    const keep = (off: () => void) => {
      if (mountedRef.current) offs.push(off);
      else off();
    };
    // Nothing from the outgoing profile may paint: reset first, then read.
    // Synchronous on purpose — deferring it would let one paint show the
    // previous profile's destinations, which is the whole failure this guards
    // against. Same exception the other profile-scoped surfaces take.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLibraries([]);
    setPicked(null);
    setPlan(null);
    setOutcome(null);
    // Including the busy flag: the in-flight call's `finally` now belongs to
    // the previous generation and will decline to clear it, which would leave
    // the card disabled for good.
    setBusy(null);
    setSurveyProgress(null);
    setUploadProgress(null);
    void (async () => {
      try {
        keep(
          await listen<UploadSurveyProgress>("remote:upload-survey", (event) => {
            if (current()) setSurveyProgress(event.payload);
          }),
        );
        keep(
          await listen<UploadProgress>("remote:upload-progress", (event) => {
            if (current()) setUploadProgress(event.payload);
          }),
        );
      } catch {
        // Progress is decoration; the card works without it.
      }
      try {
        const status = await remoteGetStatus();
        if (!current()) return;
        const nextVisible = status.signed_in && status.bootstrapped;
        setVisible(nextVisible);
        if (!nextVisible) return;
        const list = await remoteUploadLibraries();
        if (current()) {
          setLibraries(list);
          setPicked(list[0]?.library_id ?? null);
        }
      } catch {
        if (current()) setVisible(false);
      }
    })();
    return () => {
      mountedRef.current = false;
      for (const off of offs) off();
    };
  }, [activeProfileId]);

  const survey = useCallback(async () => {
    const generation = generationRef.current;
    const current = () =>
      mountedRef.current && generationRef.current === generation;
    setBusy("survey");
    setError(null);
    setOutcome(null);
    setSurveyProgress(null);
    try {
      const next = await remoteUploadSurvey();
      if (current()) setPlan(next);
    } catch (err) {
      if (current()) setError(String(err));
    } finally {
      if (current()) {
        setBusy(null);
        setSurveyProgress(null);
      }
    }
  }, []);

  const upload = useCallback(async () => {
    if (!picked || !plan || plan.candidates.length === 0) return;
    const generation = generationRef.current;
    const current = () =>
      mountedRef.current && generationRef.current === generation;
    setBusy("upload");
    setError(null);
    setUploadProgress(null);
    try {
      const next = await remoteUploadTracks(
        picked,
        plan.candidates.map((candidate) => candidate.track_id),
      );
      if (current()) {
        setOutcome(next);
        // Whatever was sent is linked now, so the plan on screen describes a
        // library that no longer exists. Re-surveying is cheap after the
        // first pass — the digests are cached — but it is the user's call.
        setPlan(null);
      }
    } catch (err) {
      if (current()) setError(String(err));
    } finally {
      if (current()) {
        setBusy(null);
        setUploadProgress(null);
      }
    }
  }, [picked, plan]);

  const cancel = useCallback(() => {
    void remoteCancelUpload().catch(() => {});
  }, []);

  if (!visible) return null;

  const running = busy !== null;
  const pct =
    uploadProgress && uploadProgress.total > 0
      ? Math.min(
          100,
          Math.round((uploadProgress.sent / uploadProgress.total) * 100),
        )
      : null;
  const refusals = new Map<string, number>();
  for (const skipped of outcome?.skipped ?? []) {
    refusals.set(skipped.reason, (refusals.get(skipped.reason) ?? 0) + 1);
  }

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start space-x-4">
        <Upload size={20} className="text-zinc-400 mt-0.5" aria-hidden="true" />
        <div className="flex-1 min-w-0 space-y-4">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h3 className="text-sm font-medium text-zinc-900 dark:text-zinc-100">
                {t("remote.upload.title")}
              </h3>
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                {t("remote.upload.subtitle")}
              </p>
            </div>
            <div className="shrink-0 flex items-center gap-2">
              {running && (
                <button
                  type="button"
                  onClick={cancel}
                  className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5"
                >
                  <X size={14} aria-hidden="true" />
                  {t("remote.upload.cancel")}
                </button>
              )}
              <button
                type="button"
                onClick={() => void survey()}
                disabled={running}
                className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
              >
                {busy === "survey" ? (
                  <Loader2
                    size={14}
                    className="animate-spin"
                    aria-hidden="true"
                  />
                ) : (
                  <Search size={14} aria-hidden="true" />
                )}
                {t("remote.upload.survey")}
              </button>
            </div>
          </div>

          {/* A running count, and here it can be a real one: the number of
              unlinked tracks is known before the first file is read. */}
          {surveyProgress && (
            <p className="text-xs text-zinc-500 dark:text-zinc-400">
              {t("remote.upload.surveying", {
                done: surveyProgress.processed,
                total: surveyProgress.total,
              })}
            </p>
          )}

          {plan && (
            <div className="space-y-3">
              <dl className="grid grid-cols-2 sm:grid-cols-4 gap-3 text-sm">
                <div>
                  <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                    {t("remote.upload.statMissing")}
                  </dt>
                  <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                    {plan.candidates.length}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                    {t("remote.upload.statLinked")}
                  </dt>
                  <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                    {plan.linked_offline}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                    {t("remote.upload.statUnsupported")}
                  </dt>
                  <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                    {plan.unsupported}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs text-zinc-500 dark:text-zinc-400">
                    {t("remote.upload.statUnreadable")}
                  </dt>
                  <dd className="tabular-nums text-zinc-900 dark:text-zinc-100">
                    {plan.unreadable}
                  </dd>
                </div>
              </dl>

              {plan.candidates.length > 0 && (
                <div className="flex items-center gap-2 flex-wrap">
                  <label
                    htmlFor="remote-upload-library"
                    className="text-xs text-zinc-500 dark:text-zinc-400"
                  >
                    {t("remote.upload.destination")}
                  </label>
                  <select
                    id="remote-upload-library"
                    value={picked ?? ""}
                    disabled={running}
                    onChange={(event) => setPicked(event.target.value)}
                    className="px-2 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 bg-white dark:bg-zinc-900 text-zinc-900 dark:text-zinc-100 disabled:opacity-50"
                  >
                    {libraries.map((library) => (
                      <option key={library.library_id} value={library.library_id}>
                        {library.name}
                      </option>
                    ))}
                  </select>
                  <button
                    type="button"
                    onClick={() => void upload()}
                    disabled={running || !picked}
                    className="px-3 py-1.5 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 inline-flex items-center gap-1.5 disabled:opacity-50"
                  >
                    {busy === "upload" ? (
                      <Loader2
                        size={14}
                        className="animate-spin"
                        aria-hidden="true"
                      />
                    ) : (
                      <Upload size={14} aria-hidden="true" />
                    )}
                    {t("remote.upload.send", { count: plan.candidates.length })}
                  </button>
                </div>
              )}
            </div>
          )}

          {busy === "upload" && (
            <div className="h-1.5 rounded-full bg-zinc-200 dark:bg-zinc-700 overflow-hidden">
              <div
                className="h-full bg-emerald-500 transition-[width]"
                style={{ width: pct === null ? "35%" : `${pct}%` }}
              />
            </div>
          )}

          {outcome && !running && (
            <div className="space-y-1">
              <p className="text-xs text-zinc-600 dark:text-zinc-300">
                {t("remote.upload.sent", { count: outcome.uploaded.length })}
                {outcome.cancelled && ` · ${t("remote.upload.stopped")}`}
              </p>
              {[...refusals.entries()].map(([reason, count]) => (
                <p
                  key={reason}
                  className="text-xs text-zinc-500 dark:text-zinc-400"
                >
                  {t(`remote.upload.refusal.${reason}`, { count })}
                </p>
              ))}
            </div>
          )}

          {error && (
            <p className="text-xs text-red-600 dark:text-red-400">{error}</p>
          )}
        </div>
      </div>
    </div>
  );
}
