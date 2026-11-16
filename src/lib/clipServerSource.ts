import {
  clipState,
  guardClipTransport,
  isCurrentClipSession,
  playClipElement,
  reportClipProgress,
  requestBackToAudio,
  unmarkClipPositioned,
} from "./clipPlayer";
import { remoteTrackClip } from "./tauri/remoteServer";

/** A ticket this close to its expiry is renewed before the element is asked
 *  to play or seek. */
const RENEW_MARGIN_MS = 60_000;

/**
 * Play a server track's clip in `element`, for `session` (RFC-029 decision
 * 9).
 *
 * The file sits behind a ticket that lasts an hour, and a clip can outlive
 * it: a long pause, then a seek, and the range request that follows meets an
 * expired ticket. The element would report that as an error whose code says
 * nothing (a 404 at load reads like a container the webview cannot decode),
 * so the renewal is decided by **the ticket's expiry**, never by an error:
 *
 * - **Before it breaks.** A play or a seek asked of a clip whose ticket has
 *   expired, or expires within a minute, mints a new one first, then points
 *   the element at it. What the user asks meanwhile wins: the latest seek's
 *   target, and the state (playing or paused) last asked for.
 * - **Only the latest mint counts.** Each carries a sequence number, and an
 *   answer that arrives after a newer mint started, or after the visit
 *   ended, is dropped.
 * - **After an error, only for an expired ticket.** An error while the
 *   ticket is past or near its expiry renews it once; with a valid ticket,
 *   the clip is one that cannot play, as a local one would be.
 * - **One renewal per incident.** An error on a renewed URL before it has
 *   played ends the visit, so renewing and failing cannot go on for ever.
 * - **A 404 from the mint** means the server no longer has the clip: it ends
 *   the visit, or at opening, says so in place of the clip. A server out of
 *   reach is reported as such.
 *
 * On Linux the URL is played as it is: WebKitGTK stops a fragmented MP4 a
 * couple of seconds in over HTTP, and a server clip does not go through the
 * loopback server that rewrites a local one.
 */
export function playServerClip(
  session: number,
  element: HTMLVideoElement,
  remoteTrackId: string,
): void {
  let expiresAt = 0;
  let sequence = 0;
  // A mint is in flight: the element is about to be pointed elsewhere, and
  // what the user asks is held for it.
  let minting = false;
  // The element was pointed at a renewed URL and has not played since.
  let renewedSincePlaying = false;
  // What the user asked last, applied once the new URL loads.
  let wantPositionMs = 0;
  let wantPlaying = false;

  const current = () => isCurrentClipSession(session);
  const expiring = () => Date.now() >= expiresAt - RENEW_MARGIN_MS;

  function mint(renewal: boolean, positionMs: number, playing: boolean) {
    sequence += 1;
    const mine = sequence;
    minting = true;
    wantPositionMs = positionMs;
    wantPlaying = playing;
    reportClipProgress(session, { positionMs, paused: !playing });
    remoteTrackClip(remoteTrackId).then(
      (ticket) => {
        if (mine !== sequence || !current()) return;
        minting = false;
        if (ticket === null) {
          if (renewal) requestBackToAudio("user");
          else reportClipProgress(session, { failed: "gone" });
          return;
        }
        expiresAt = ticket.expiresAt;
        renewedSincePlaying = renewal;
        // The element's own handler puts it at the store's position and
        // plays it unless the store says paused; this one holds it to the
        // state last asked for, whatever the element reported meanwhile.
        reportClipProgress(session, {
          positionMs: wantPositionMs,
          paused: !wantPlaying,
        });
        unmarkClipPositioned(session, element);
        element.addEventListener(
          "loadedmetadata",
          () => {
            if (!current()) return;
            if (wantPlaying) playClipElement(session, element);
            else element.pause();
          },
          { once: true },
        );
        element.src = ticket.url;
      },
      (err: unknown) => {
        if (mine !== sequence || !current()) return;
        minting = false;
        console.error("[clipServerSource] clip ticket failed", err);
        reportClipProgress(session, { failed: "unreachable" });
      },
    );
  }

  guardClipTransport(session, {
    busy: () => minting,
    play() {
      if (minting) {
        wantPlaying = true;
        reportClipProgress(session, { paused: false });
        return true;
      }
      if (!expiring()) return false;
      mint(true, clipState().positionMs, true);
      return true;
    },
    pause() {
      if (!minting) return false;
      wantPlaying = false;
      reportClipProgress(session, { paused: true });
      // The element still playing the old URL, if it was.
      element.pause();
      return true;
    },
    seek(ms) {
      if (minting) {
        wantPositionMs = ms;
        reportClipProgress(session, { positionMs: ms });
        return true;
      }
      if (!expiring()) return false;
      mint(true, ms, !clipState().paused);
      return true;
    },
  });

  element.addEventListener("playing", () => {
    renewedSincePlaying = false;
  });
  element.addEventListener("error", () => {
    // A release empties the element, and a mint in flight replaces it.
    if (!current() || minting || !element.getAttribute("src")) return;
    if (renewedSincePlaying) {
      requestBackToAudio("user");
    } else if (expiring()) {
      const { positionMs, paused } = clipState();
      mint(true, positionMs, !paused);
    } else {
      reportClipProgress(session, { failed: "unplayable" });
    }
  });

  const { positionMs, paused } = clipState();
  mint(false, positionMs, !paused);
}
