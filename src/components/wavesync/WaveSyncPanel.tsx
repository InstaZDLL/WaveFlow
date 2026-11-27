import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  AlertTriangle,
  Check,
  Copy,
  Crown,
  KeyRound,
  Loader2,
  Play,
  RefreshCw,
  Trash2,
  UserMinus,
  X,
} from "lucide-react";

import { usePlayer } from "../../hooks/usePlayer";
import {
  openWaveSyncDialog,
  showWaveSyncNotice,
  useWaveSync,
} from "../../hooks/useWaveSync";
import { backendErrorText } from "../../lib/backendError";
import { playerPlayTracks } from "../../lib/tauri/player";
import { formatDuration } from "../../lib/tauri/track";
import {
  wavesyncApprove,
  wavesyncDismiss,
  wavesyncLeave,
  wavesyncNewCode,
  wavesyncRemove,
  wavesyncRemoveParticipant,
  type WaveSyncEntry,
  type WaveSyncView,
} from "../../lib/tauri/wavesync";

/** After this long with nobody in, the host is told the firewall may be why. */
const LONELY_MS = 60_000;
const ROW_HEIGHT = 60;

/**
 * The WaveSync panel (RFC-010 decision 7): who is in the session, what the
 * queue holds and who added it, and for the host the code, the invitation
 * and the requests to join. A right panel like the queue, mutually
 * exclusive with the others through `PlayerContext`.
 */
export function WaveSyncPanel({ embedded = false }: { embedded?: boolean }) {
  const { t } = useTranslation();
  const { view } = useWaveSync();
  const { closeRightPanel } = usePlayer();

  const body = (
    <div
      className={
        embedded
          ? "flex h-full flex-col px-6 pb-6 pt-3"
          : "flex h-full w-80 flex-col p-6"
      }
    >
      <div
        className={`flex items-start justify-between ${embedded ? "mb-4" : "mb-5"}`}
      >
        <div className="min-w-0">
          {!embedded && <h2 className="text-xl font-bold">WaveSync</h2>}
          {view?.sessionName && (
            <p className="mt-1 truncate text-xs text-zinc-500">
              {view.sessionName}
              <span className="ms-2 rounded-full bg-zinc-200 px-2 py-0.5 text-[10px] font-medium dark:bg-zinc-700">
                {t(
                  view.role === "host"
                    ? "wavesync.panel.hosting"
                    : "wavesync.panel.joined",
                )}
              </span>
            </p>
          )}
        </div>
        {!embedded && (
          <button
            type="button"
            onClick={closeRightPanel}
            aria-label={t("common.close")}
            className="rounded-full p-2 transition-colors hover:bg-zinc-100 dark:hover:bg-zinc-800"
          >
            <X size={20} />
          </button>
        )}
      </div>
      <PanelBody view={view} />
    </div>
  );

  if (embedded) return <div className="h-full">{body}</div>;
  return (
    <motion.aside
      key="wavesync"
      initial={{ width: 0, opacity: 0 }}
      animate={{ width: 320, opacity: 1 }}
      exit={{ width: 0, opacity: 0 }}
      transition={{ type: "spring", stiffness: 320, damping: 32, mass: 0.8 }}
      className="h-full shrink-0 overflow-hidden border-s border-zinc-200 bg-surface-light text-zinc-800 dark:border-zinc-800 dark:bg-surface-dark dark:text-zinc-100"
    >
      {body}
    </motion.aside>
  );
}

function PanelBody({ view }: { view: WaveSyncView | null }) {
  const { t } = useTranslation();
  if (view == null) return null;

  if (view.status === "idle") {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
        {view.endedReason && (
          <p role="status" className="text-sm text-zinc-600 dark:text-zinc-300">
            {t(`wavesync.ended.${view.endedReason}`)}
          </p>
        )}
        <button
          type="button"
          onClick={() => {
            void wavesyncDismiss();
            openWaveSyncDialog();
          }}
          className="rounded-xl bg-emerald-500 px-4 py-2 text-sm font-semibold text-white hover:bg-emerald-400"
        >
          {t("wavesync.panel.startOrJoin")}
        </button>
      </div>
    );
  }

  if (view.status === "waiting") {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
        <Loader2
          size={24}
          className="animate-spin text-emerald-500"
          aria-hidden="true"
        />
        <p role="status" className="text-sm text-zinc-600 dark:text-zinc-300">
          {t("wavesync.panel.waiting")}
        </p>
        <LeaveButton view={view} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4">
      {view.role === "host" && <HostBox view={view} />}
      <Participants view={view} />
      <Queue view={view} />
      <LeaveButton view={view} />
    </div>
  );
}

function HostBox({ view }: { view: WaveSyncView }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // The firewall hint shows after a minute alone: tick until then.
  useEffect(() => {
    if (view.everJoined) return;
    const timer = window.setInterval(() => setNow(Date.now()), 5_000);
    return () => window.clearInterval(timer);
  }, [view.everJoined]);

  const lonely =
    !view.everJoined &&
    view.startedAtMs != null &&
    now - view.startedAtMs > LONELY_MS;

  const copyInvitation = async () => {
    if (!view.invitation) return;
    try {
      await navigator.clipboard.writeText(view.invitation);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2_000);
    } catch (err) {
      console.error("[wavesync] copy", err);
    }
  };

  return (
    <div className="shrink-0 space-y-3">
      <div className="rounded-xl border border-emerald-100 bg-emerald-50 p-3 dark:border-emerald-900/40 dark:bg-emerald-900/20">
        <div className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-widest text-emerald-700 dark:text-emerald-300">
            <KeyRound size={12} aria-hidden="true" />
            {t("wavesync.panel.code")}
          </span>
          <button
            type="button"
            onClick={() => void wavesyncNewCode()}
            aria-label={t("wavesync.panel.newCode")}
            title={t("wavesync.panel.newCode")}
            className="rounded-full p-1 text-emerald-700 hover:bg-emerald-100 dark:text-emerald-300 dark:hover:bg-emerald-900/40"
          >
            <RefreshCw size={13} />
          </button>
        </div>
        <p
          dir="ltr"
          className="mt-1 font-mono text-2xl font-bold tracking-[0.2em] text-zinc-900 dark:text-white"
        >
          {view.code}
        </p>
        {view.fingerprint && (
          <p className="mt-1 text-[11px] text-zinc-500">
            {t("wavesync.panel.fingerprint")}{" "}
            <span dir="ltr" className="font-mono">
              {view.fingerprint}
            </span>
          </p>
        )}
        {view.invitation && (
          <button
            type="button"
            onClick={() => void copyInvitation()}
            className="mt-2 flex items-center gap-1.5 text-xs font-medium text-emerald-700 hover:underline dark:text-emerald-300"
          >
            {copied ? <Check size={12} /> : <Copy size={12} />}
            {t(
              copied
                ? "wavesync.panel.copied"
                : "wavesync.panel.copyInvitation",
            )}
          </button>
        )}
      </div>

      {view.pending.map((request) => (
        <div
          key={request.request}
          role="group"
          aria-label={t("wavesync.panel.wantsToJoin", { name: request.name })}
          className="rounded-xl border border-zinc-200 p-3 dark:border-zinc-700"
        >
          <p className="mb-2 text-sm text-zinc-800 dark:text-zinc-100">
            {t("wavesync.panel.wantsToJoin", { name: request.name })}
          </p>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => void wavesyncApprove(request.request, true)}
              className="rounded-lg bg-emerald-500 px-3 py-1.5 text-xs font-semibold text-white hover:bg-emerald-400"
            >
              {t("wavesync.panel.letIn")}
            </button>
            <button
              type="button"
              onClick={() => void wavesyncApprove(request.request, false)}
              className="rounded-lg px-3 py-1.5 text-xs font-medium text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
            >
              {t("wavesync.panel.turnAway")}
            </button>
          </div>
        </div>
      ))}

      {view.wrongCodes > 0 && (
        <Warning
          text={t("wavesync.panel.wrongCodes", { count: view.wrongCodes })}
        />
      )}
      {lonely && <Warning text={t("wavesync.panel.firewall")} />}
    </div>
  );
}

function Warning({ text }: { text: string }) {
  return (
    <p className="flex items-start gap-2 rounded-xl bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:bg-amber-900/20 dark:text-amber-200">
      <AlertTriangle size={14} className="mt-0.5 shrink-0" aria-hidden="true" />
      <span>{text}</span>
    </p>
  );
}

function Participants({ view }: { view: WaveSyncView }) {
  const { t } = useTranslation();
  const hosting = view.role === "host";
  return (
    <section className="shrink-0" aria-label={t("wavesync.panel.participants")}>
      <h3 className="mb-2 text-[10px] font-bold uppercase tracking-widest text-zinc-500">
        {t("wavesync.panel.participants")} · {view.participants.length}
      </h3>
      <ul className="space-y-1">
        {view.participants.map((p) => (
          <li key={p.id} className="flex items-center gap-2 text-sm">
            <span
              aria-hidden="true"
              className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-zinc-200 text-[11px] font-bold dark:bg-zinc-700"
            >
              {p.name.slice(0, 1).toUpperCase()}
            </span>
            <span className="min-w-0 flex-1 truncate">
              {p.name}
              {p.me && (
                <span className="text-zinc-500">
                  {" "}
                  · {t("wavesync.panel.you")}
                </span>
              )}
            </span>
            {p.host && (
              <Crown
                size={13}
                className="shrink-0 text-amber-500"
                aria-label={t("wavesync.panel.host")}
              />
            )}
            {p.state === "away" && (
              <span className="shrink-0 text-[11px] text-zinc-500">
                {t("wavesync.panel.away")}
              </span>
            )}
            {hosting && !p.host && (
              <button
                type="button"
                onClick={() => void wavesyncRemoveParticipant(p.id)}
                aria-label={t("wavesync.panel.removeParticipant", {
                  name: p.name,
                })}
                title={t("wavesync.panel.removeParticipant", { name: p.name })}
                className="shrink-0 rounded-full p-1 text-zinc-400 hover:bg-zinc-100 hover:text-red-500 dark:hover:bg-zinc-800"
              >
                <UserMinus size={13} />
              </button>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

function Queue({ view }: { view: WaveSyncView }) {
  const { t } = useTranslation();
  const scrollRef = useRef<HTMLDivElement>(null);
  const rows = view.queue;
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 6,
  });

  return (
    <section
      className="flex min-h-0 flex-1 flex-col"
      aria-label={t("wavesync.panel.queue")}
    >
      <h3 className="mb-2 shrink-0 text-[10px] font-bold uppercase tracking-widest text-zinc-500">
        {t("wavesync.panel.queue")} · {rows.length}
      </h3>
      {rows.length === 0 ? (
        <p className="text-sm text-zinc-500">
          {t("wavesync.panel.emptyQueue")}
        </p>
      ) : (
        <div
          ref={scrollRef}
          className="min-h-0 flex-1 overflow-y-auto scrollbar-hide"
        >
          <ul
            className="relative w-full"
            style={{ height: `${rowVirtualizer.getTotalSize()}px` }}
          >
            {rowVirtualizer.getVirtualItems().map((item) => (
              <li
                key={rows[item.index].id}
                className="absolute inset-x-0"
                style={{
                  height: ROW_HEIGHT,
                  transform: `translateY(${item.start}px)`,
                }}
              >
                <QueueRow
                  entry={rows[item.index]}
                  hosting={view.role === "host"}
                />
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function QueueRow({
  entry,
  hosting,
}: {
  entry: WaveSyncEntry;
  hosting: boolean;
}) {
  const { t } = useTranslation();
  const canRemove = entry.state !== "played" && (entry.mine || hosting);
  const by = entry.mine
    ? t("wavesync.panel.addedByYou")
    : t("wavesync.panel.addedBy", { name: entry.ownerName ?? "?" });
  const note =
    entry.state === "ownerAway"
      ? t("wavesync.panel.ownerAway")
      : !entry.mine && entry.state !== "played"
        ? t("wavesync.panel.ownerOnly")
        : null;

  const play = async () => {
    if (entry.trackId == null) return;
    try {
      await playerPlayTracks("manual", null, [entry.trackId], 0);
    } catch (err) {
      showWaveSyncNotice(backendErrorText(err, t));
    }
  };
  const remove = async () => {
    try {
      await wavesyncRemove(entry.id);
    } catch (err) {
      showWaveSyncNotice(backendErrorText(err, t));
    }
  };

  return (
    <div
      className={`flex h-full items-center gap-2 rounded-lg px-2 ${
        entry.state === "played" ? "opacity-50" : ""
      }`}
    >
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-zinc-900 dark:text-white">
          {entry.title}
        </p>
        <p className="truncate text-xs text-zinc-500">
          {entry.artists.join(", ")}
          {entry.artists.length > 0 && " · "}
          {by}
        </p>
        {note && <p className="truncate text-[11px] text-zinc-400">{note}</p>}
      </div>
      <span
        dir="ltr"
        className="shrink-0 text-[11px] tabular-nums text-zinc-400"
      >
        {formatDuration(entry.durationMs)}
      </span>
      {entry.mine && entry.trackId != null && (
        <button
          type="button"
          onClick={() => void play()}
          aria-label={t("wavesync.panel.play", { title: entry.title })}
          title={t("wavesync.panel.play", { title: entry.title })}
          className="shrink-0 rounded-full p-1.5 text-zinc-500 hover:bg-zinc-100 hover:text-emerald-500 dark:hover:bg-zinc-800"
        >
          <Play size={14} />
        </button>
      )}
      {canRemove && (
        <button
          type="button"
          onClick={() => void remove()}
          aria-label={t("wavesync.panel.remove", { title: entry.title })}
          title={t("wavesync.panel.remove", { title: entry.title })}
          className="shrink-0 rounded-full p-1.5 text-zinc-400 hover:bg-zinc-100 hover:text-red-500 dark:hover:bg-zinc-800"
        >
          <Trash2 size={14} />
        </button>
      )}
    </div>
  );
}

function LeaveButton({ view }: { view: WaveSyncView }) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={() => void wavesyncLeave()}
      className="shrink-0 rounded-xl border border-zinc-200 px-4 py-2 text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800"
    >
      {t(view.role === "host" ? "wavesync.panel.end" : "wavesync.panel.leave")}
    </button>
  );
}
