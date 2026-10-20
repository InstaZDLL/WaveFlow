import { ArrowUpRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useTrackLyrics } from "../../../hooks/useTrackLyrics";

/** How many lines the card shows: the one being sung and its neighbours. */
const PREVIEW_LINES = 3;

/**
 * A glimpse of the lyrics in the Details tab of the Fluid panel: the
 * line being sung with the one before and the one after, or the first
 * lines of an unsynced text, on a quiet card that opens the Lyrics tab.
 * Mounted only inside that tab, so the second fetch path the hook opens
 * exists only there, and the hook's own cache answers it.
 */
export function LyricsPreviewCard({ onOpen }: { onOpen: () => void }) {
  const { t } = useTranslation();
  const { payload, lrcLines, isSynced, activeIndex, radioPlainText, isRadio } =
    useTrackLyrics();

  let lines: { text: string; active: boolean }[] = [];
  if (isSynced && lrcLines.length > 0) {
    // Around the active line, or the opening lines before the first one.
    const start =
      activeIndex < 0
        ? 0
        : Math.max(
            0,
            Math.min(activeIndex - 1, lrcLines.length - PREVIEW_LINES),
          );
    lines = lrcLines
      .slice(start, start + PREVIEW_LINES)
      .map((line, i) => ({
        text: line.text,
        active: start + i === activeIndex,
      }))
      .filter((l) => l.text.trim() !== "");
  } else {
    const text = isRadio ? radioPlainText : payload?.content;
    if (text) {
      lines = text
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter(Boolean)
        .slice(0, PREVIEW_LINES)
        .map((l) => ({ text: l, active: false }));
    }
  }
  if (lines.length === 0) return null;

  return (
    <button
      type="button"
      onClick={onOpen}
      className="fe-panel__lyrics block w-full text-left"
    >
      <p className="fe-label mb-2">{t("lyrics.title")}</p>
      {lines.map((line, i) => (
        <p
          key={i}
          className={`truncate text-[0.9375rem] leading-relaxed ${
            line.active ? "fe-ink font-[540]" : "fe-ink-3"
          }`}
        >
          {line.text}
        </p>
      ))}
      <span className="fe-link mt-3 inline-flex items-center gap-1 text-[0.8125rem]">
        {t("nowPlaying.fluid.openLyrics")}
        <ArrowUpRight size={13} strokeWidth={2} />
      </span>
    </button>
  );
}
