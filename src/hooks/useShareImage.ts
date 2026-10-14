import { useState } from "react";
import { pickSaveFile } from "../lib/tauri/dialog";
import { saveShareImage } from "../lib/tauri/share";

export type ShareImageState = "idle" | "saving" | "copying" | "done";

/**
 * Save a rendered share card through the native dialog, or copy it to the
 * clipboard. The card is only built once the user has chosen, and only if
 * they did not cancel the dialog. `state` drives the trigger's spinner and
 * its brief tick.
 *
 * Shared by every share button (immersive view, playlist, artist): the
 * flow, the filename sanitising and the error handling used to live in the
 * one button that had it.
 */
export function useShareImage(
  buildCard: () => Promise<Blob>,
  /** What the default filename starts with: a track, playlist or artist
   *  name. Sanitised here. */
  fileStem: string,
) {
  const [state, setState] = useState<ShareImageState>("idle");

  const finish = () => {
    setState("done");
    window.setTimeout(() => setState("idle"), 2000);
  };

  const save = async () => {
    try {
      setState("saving");
      const target = await pickSaveFile(
        `${sanitizeFilename(fileStem)} - WaveFlow.png`,
        ["png"],
      );
      if (!target) {
        setState("idle");
        return;
      }
      const blob = await buildCard();
      await saveShareImage(new Uint8Array(await blob.arrayBuffer()), target);
      finish();
    } catch (err) {
      console.error("[useShareImage] save failed", err);
      setState("idle");
    }
  };

  const copy = async () => {
    try {
      setState("copying");
      const blob = await buildCard();
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blob }),
      ]);
      finish();
    } catch (err) {
      console.error("[useShareImage] copy failed", err);
      setState("idle");
    }
  };

  return {
    state,
    busy: state === "saving" || state === "copying",
    save,
    copy,
  };
}

/** The native save dialog rejects `< > : " / \ | ? *` on Windows, and a
 *  name stripped of them beats an opaque "card.png". */
function sanitizeFilename(s: string): string {
  return (
    s
      // eslint-disable-next-line no-control-regex
      .replace(/[<>:"/\\|?*\x00-\x1f]/g, "_")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, 80) || "WaveFlow"
  );
}
