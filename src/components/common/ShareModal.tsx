import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Link2, Loader2, Trash2 } from "lucide-react";
import QRCode from "qrcode";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "./AnimatedModalShell";
import {
  shareLinkMint,
  shareLinkRevoke,
  shareLinkStatus,
  type ShareLink,
} from "../../lib/tauri/share";

interface ShareModalProps {
  playlistId: number;
  playlistName: string;
  isOpen: boolean;
  onClose: () => void;
}

/**
 * Public share-link manager. Phase 1.g.3-desktop.
 *
 * Opens against the cached status (instant), then exposes mint /
 * revoke buttons that hit the waveflow-server canonical-share
 * endpoints. The QR code is generated client-side on every URL
 * change so an offline user still gets a scannable code.
 *
 * The modal does NOT auto-mint on open — the user must press
 * "Generate link" to publish. Same UX as Spotify / Apple Music
 * where a share starts as opt-in rather than always-on.
 */
export function ShareModal({
  playlistId,
  playlistName,
  isOpen,
  onClose,
}: ShareModalProps) {
  const { t } = useTranslation();
  const [link, setLink] = useState<ShareLink | null>(null);
  const [isMinting, setIsMinting] = useState(false);
  const [isRevoking, setIsRevoking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  const headingId = useId();
  // Bumped on every close — pending async writes check it before
  // committing their result so a closed-and-reopened modal doesn't
  // inherit stale state from a previous playlist.
  const sessionRef = useRef(0);
  const copyTimeoutRef = useRef<number | null>(null);

  useEffect(() => {
    if (!isOpen) {
      sessionRef.current++;
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setLink(null);
      setError(null);
      setIsMinting(false);
      setIsRevoking(false);
      setCopied(false);
      setQrDataUrl(null);
      if (copyTimeoutRef.current !== null) {
        window.clearTimeout(copyTimeoutRef.current);
        copyTimeoutRef.current = null;
      }
      return;
    }

    const session = ++sessionRef.current;
    shareLinkStatus(playlistId)
      .then((status) => {
        if (session !== sessionRef.current) return;
        setLink(status.link);
      })
      .catch((err) => {
        if (session !== sessionRef.current) return;
        setError(String(err));
      });
  }, [isOpen, playlistId]);

  // Regenerate the QR every time the URL changes.
  useEffect(() => {
    if (!link) {
      // Mirror the close-effect reset rule (same lint guard): the
      // post-revoke clear happens in `handleRevoke`'s success path,
      // so the effect doesn't need to re-clear here.
      return;
    }
    const session = sessionRef.current;
    QRCode.toDataURL(link.url, {
      errorCorrectionLevel: "M",
      margin: 1,
      width: 220,
      color: { dark: "#0b0d11", light: "#ffffff" },
    })
      .then((dataUrl) => {
        if (session !== sessionRef.current) return;
        setQrDataUrl(dataUrl);
      })
      .catch(() => {
        // QR generation failure is non-fatal — the URL + copy
        // button still work. Leave the placeholder slot empty.
        if (session !== sessionRef.current) return;
        setQrDataUrl(null);
      });
  }, [link]);

  const handleMint = useCallback(async () => {
    setIsMinting(true);
    setError(null);
    // Bump BEFORE the await so any in-flight `shareLinkStatus` from
    // the open-effect resolves into a stale session and short-
    // circuits in its own .then() guard. Without this, a slow
    // status call landing after a quick mint would overwrite the
    // freshly-minted link with the cached (null) value.
    const session = ++sessionRef.current;
    try {
      const minted = await shareLinkMint(playlistId);
      if (session !== sessionRef.current) return;
      setLink(minted);
    } catch (err) {
      if (session !== sessionRef.current) return;
      setError(String(err));
    } finally {
      if (session === sessionRef.current) {
        setIsMinting(false);
      }
    }
  }, [playlistId]);

  const handleRevoke = useCallback(async () => {
    setIsRevoking(true);
    setError(null);
    // Same race protection as `handleMint` — pre-bump invalidates
    // any in-flight status read before the network round-trip.
    const session = ++sessionRef.current;
    try {
      await shareLinkRevoke(playlistId);
      if (session !== sessionRef.current) return;
      setLink(null);
      // QR effect only runs when `link` becomes non-null, so the
      // stale data URL has to be cleared explicitly here. Keeps
      // the effect free of nested setState (lint guard).
      setQrDataUrl(null);
    } catch (err) {
      if (session !== sessionRef.current) return;
      setError(String(err));
    } finally {
      if (session === sessionRef.current) {
        setIsRevoking(false);
      }
    }
  }, [playlistId]);

  const handleCopy = useCallback(async () => {
    if (!link) return;
    try {
      await navigator.clipboard.writeText(link.url);
      setCopied(true);
      if (copyTimeoutRef.current !== null) {
        window.clearTimeout(copyTimeoutRef.current);
      }
      copyTimeoutRef.current = window.setTimeout(() => {
        setCopied(false);
        copyTimeoutRef.current = null;
      }, 1800);
    } catch {
      // Clipboard write rejected (permission, locked focus). Surface
      // a friendly message — the URL is still visible + selectable
      // in the input above.
      setError(t("share.errorClipboard"));
    }
  }, [link, t]);

  return (
    <AnimatedModalShell isOpen={isOpen} onBackdropClick={onClose}>
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={headingId}
        className="bg-white dark:bg-surface-dark text-zinc-900 dark:text-zinc-50 rounded-2xl shadow-2xl max-w-md w-full p-6 max-h-[calc(100vh-2rem)] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={headingId} className="text-xl font-semibold mb-1">
          {t("share.title")}
        </h2>
        <p className="text-sm text-zinc-500 dark:text-zinc-400 mb-5">
          {t("share.subtitle", { name: playlistName })}
        </p>

        {error && (
          <div
            role="alert"
            className="mb-4 rounded-lg border border-rose-300 bg-rose-50 dark:border-rose-900 dark:bg-rose-950/50 px-3 py-2 text-sm text-rose-900 dark:text-rose-200"
          >
            {error}
          </div>
        )}

        {link ? (
          <div className="space-y-4">
            <div className="flex justify-center">
              {qrDataUrl ? (
                <img
                  src={qrDataUrl}
                  alt={t("share.qrAlt")}
                  className="rounded-lg border border-zinc-200 dark:border-zinc-800"
                  width={220}
                  height={220}
                />
              ) : (
                <div className="w-55 h-55 rounded-lg border border-zinc-200 dark:border-zinc-800 flex items-center justify-center">
                  <Loader2 className="w-6 h-6 animate-spin text-zinc-400" />
                </div>
              )}
            </div>

            <div className="flex items-stretch gap-2">
              <input
                type="text"
                readOnly
                value={link.url}
                aria-label={t("share.urlLabel")}
                className="flex-1 min-w-0 rounded-lg border border-zinc-300 dark:border-zinc-700 bg-zinc-50 dark:bg-zinc-900/60 px-3 py-2 text-sm font-mono"
                onFocus={(e) => e.currentTarget.select()}
              />
              <button
                type="button"
                onClick={handleCopy}
                className="inline-flex items-center justify-center gap-1.5 rounded-lg bg-violet-500 hover:bg-violet-400 text-white px-3 py-2 text-sm font-medium transition-colors min-w-22"
              >
                {copied ? (
                  <>
                    <Check className="w-4 h-4" /> {t("share.copied")}
                  </>
                ) : (
                  <>
                    <Copy className="w-4 h-4" /> {t("share.copy")}
                  </>
                )}
              </button>
            </div>

            <div className="flex items-center justify-between pt-2 border-t border-zinc-200 dark:border-zinc-800">
              <p className="text-xs text-zinc-500 dark:text-zinc-400">
                {t("share.activeHint")}
              </p>
              <button
                type="button"
                onClick={handleRevoke}
                disabled={isRevoking}
                className="inline-flex items-center gap-1.5 rounded-lg border border-rose-300 dark:border-rose-900 bg-rose-50 dark:bg-rose-950/40 hover:bg-rose-100 dark:hover:bg-rose-900/40 text-rose-700 dark:text-rose-200 px-3 py-1.5 text-sm font-medium transition-colors disabled:opacity-60"
              >
                {isRevoking ? (
                  <Loader2 className="w-4 h-4 animate-spin" />
                ) : (
                  <Trash2 className="w-4 h-4" />
                )}
                {t("share.revoke")}
              </button>
            </div>
          </div>
        ) : (
          <div className="space-y-4">
            <div className="rounded-lg bg-zinc-50 dark:bg-zinc-900/60 px-4 py-3 text-sm text-zinc-600 dark:text-zinc-300">
              {t("share.idleHint")}
            </div>
            <button
              type="button"
              onClick={handleMint}
              disabled={isMinting}
              className="inline-flex w-full items-center justify-center gap-2 rounded-lg bg-violet-500 hover:bg-violet-400 text-white px-4 py-2.5 text-sm font-medium transition-colors disabled:opacity-60"
            >
              {isMinting ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Link2 className="w-4 h-4" />
              )}
              {t("share.mint")}
            </button>
          </div>
        )}

        <div className="mt-6 flex justify-end">
          <button
            type="button"
            onClick={onClose}
            className="rounded-lg border border-zinc-300 dark:border-zinc-700 px-3 py-1.5 text-sm font-medium hover:bg-zinc-50 dark:hover:bg-zinc-800/60 transition-colors"
          >
            {t("share.close")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
