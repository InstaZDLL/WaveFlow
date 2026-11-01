import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { useTranslation } from "react-i18next";
import { ArrowRight, ChevronLeft, ChevronRight, X } from "lucide-react";

import {
  AnimatedModalContent,
  AnimatedModalShell,
} from "../common/AnimatedModalShell";
import { useLibrary } from "../../hooks/useLibrary";
import { useModalA11y } from "../../hooks/useModalA11y";
import { usePlayer } from "../../hooks/usePlayer";
import { useWhatsNewAutoOpen, useWindowFocused } from "../../hooks/useWhatsNew";
import {
  openPlaylistImport,
  openView,
  requestLyricsEditor,
} from "../../lib/discovery";
import {
  isFileTrack,
  isRadioTrack,
  isRemoteTrack,
} from "../../lib/playerSources";
import { listAlbums } from "../../lib/tauri/browse";
import { openTagWorkshop } from "../../lib/tagWorkshop";
import type { WhatsNewPage, WhatsNewRelease } from "../../lib/whatsNew";
import { FeatureHero } from "./FeatureHero";
import { shuffledLibrary } from "./shuffledLibrary";

interface WhatsNewPanelProps {
  isOpen: boolean;
  release: WhatsNewRelease;
  onClose: () => void;
}

/**
 * What changed in this version, page by page: a large illustration, a few
 * lines, and a button that goes to the feature. Opened once after an update
 * by [`useWhatsNew`], and on demand from the top bar, Settings and About.
 *
 * The pages come from [`whatsNew.ts`], written by hand per version. No
 * `<header>` or `<footer>` in here: the fluid skin styles those tags as the
 * top bar and the player bar.
 */
export function WhatsNewPanel({
  isOpen,
  release,
  onClose,
}: WhatsNewPanelProps) {
  const { t } = useTranslation();
  const titleId = useId();
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, onClose);
  const nextRef = useRef<HTMLButtonElement>(null);
  const [index, setIndex] = useState(0);
  const [wasOpen, setWasOpen] = useState(isOpen);
  if (wasOpen !== isOpen) {
    setWasOpen(isOpen);
    if (isOpen) setIndex(0);
  }

  const { libraries } = useLibrary();
  const { currentTrack, openRightPanel, openTheater, playTracks } = usePlayer();
  const autoOpen = useWhatsNewAutoOpen();
  const focused = useWindowFocused();

  const holdsMusic = libraries.some((library) => library.track_count > 0);
  // The editor is keyed on the library row, which these do not have.
  const hasLibraryTrack =
    currentTrack != null &&
    !isRadioTrack(currentTrack) &&
    !isRemoteTrack(currentTrack) &&
    !isFileTrack(currentTrack);

  // After the hook's own focus, which goes to the first control (the close
  // button): Enter and Space should carry on through the pages.
  useEffect(() => {
    if (!isOpen) return;
    const timer = window.setTimeout(() => nextRef.current?.focus(), 0);
    return () => window.clearTimeout(timer);
  }, [isOpen]);

  const pages = release.pages;
  const page = pages[Math.min(index, pages.length - 1)];
  const isFirst = index === 0;
  const isLast = index >= pages.length - 1;

  const goTo = (next: number) => {
    setIndex(Math.max(0, Math.min(pages.length - 1, next)));
  };

  // The arrows follow the reading direction. Every other key stops here too,
  // except what the modal hook listens for on `document`: a key pressed in
  // the panel is not the player's, and Space on a focused button must not
  // also pause the music.
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape" || event.key === "Tab") return;
    event.stopPropagation();
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const rtl = document.documentElement.dir === "rtl";
    const forward = event.key === (rtl ? "ArrowLeft" : "ArrowRight");
    goTo(index + (forward ? 1 : -1));
  };

  /** What this page can offer right now: its feature, or what it needs. */
  const blocker = (target: WhatsNewPage): "library" | "track" | null => {
    if (!target.requires) return null;
    if (!holdsMusic) return "library";
    if (target.requires === "track" && !hasLibraryTrack) return "track";
    return null;
  };

  /** The line under the text, when the button will not do the obvious. */
  const noteFor = (target: WhatsNewPage): string | null => {
    const needs = blocker(target);
    if (needs === "library") return t("whatsNew.needs.library");
    if (needs === "track") return t("whatsNew.needs.track");
    if (target.action.kind === "theater" && !currentTrack) {
      return t("whatsNew.needs.startsPlayback");
    }
    return null;
  };

  const run = async (target: WhatsNewPage) => {
    onClose();
    if (blocker(target) != null) {
      openView({ view: "library" });
      return;
    }
    const action = target.action;
    try {
      switch (action.kind) {
        case "view":
          openView(action.request);
          return;
        case "lyricsEditor":
          openRightPanel("lyrics");
          requestLyricsEditor();
          return;
        case "theater":
          // The view shows what plays: with nothing playing, start the
          // library first, which the page has said it would.
          if (!currentTrack) {
            const tracks = await shuffledLibrary();
            if (tracks.length === 0) {
              openView({ view: "library" });
              return;
            }
            await playTracks(tracks, 0, { type: "library", id: null });
          }
          openTheater();
          return;
        case "tagWorkshop": {
          // The album playing, else the one added last.
          let albumId = currentTrack?.album_id ?? null;
          if (albumId == null) {
            const albums = await listAlbums(null, {
              orderBy: "added_at",
              direction: "desc",
            });
            albumId = albums[0]?.id ?? null;
          }
          if (albumId == null) openView({ view: "library" });
          else
            openTagWorkshop({ source: { kind: "album", album_id: albumId } });
          return;
        }
        case "playlistImport":
          openPlaylistImport();
          return;
      }
    } catch (err) {
      console.error(`[WhatsNewPanel] ${target.id} failed`, err);
    }
  };

  const keywords = useMemo(
    () =>
      t(`whatsNew.pages.${page.id}.keywords`)
        .split("|")
        .map((word) => word.trim())
        .filter(Boolean),
    [t, page.id],
  );

  const needs = blocker(page);
  return (
    <AnimatedModalShell
      isOpen={isOpen}
      onBackdropClick={onClose}
      backdropClassName="fixed inset-0 z-100 flex items-center justify-center bg-black/70 p-4"
    >
      <AnimatedModalContent
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onKeyDown={onKeyDown}
        data-paused={focused ? undefined : ""}
        className="whats-new flex max-h-[calc(100vh-2rem)] w-full max-w-2xl flex-col overflow-hidden rounded-3xl border border-zinc-200 bg-white shadow-2xl dark:border-zinc-800 dark:bg-zinc-900"
      >
        <div className="flex shrink-0 items-center gap-3 px-6 pb-3 pt-4">
          <h2
            id={titleId}
            className="whats-new__heading min-w-0 flex-1 truncate text-base font-semibold text-zinc-900 dark:text-white"
          >
            {t("whatsNew.title", { version: release.label })}
          </h2>
          <span
            dir="ltr"
            aria-label={t("whatsNew.pageOf", {
              current: index + 1,
              total: pages.length,
            })}
            className="whats-new__count shrink-0 text-xs tabular-nums text-zinc-500 dark:text-zinc-400"
          >
            {index + 1} / {pages.length}
          </span>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("whatsNew.close")}
            className="whats-new__ghost shrink-0 rounded-full p-1.5 text-zinc-400 transition-colors hover:bg-zinc-100 hover:text-zinc-700 dark:hover:bg-zinc-800 dark:hover:text-zinc-200"
          >
            <X size={18} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-6">
          <div key={page.id} className="whats-new__art">
            <FeatureHero
              art={page.art}
              color={page.color}
              keywords={keywords}
            />
          </div>
          {/* Every page's text in one cell, only one of them shown: the
              panel takes the height of the longest, in whatever language,
              so the controls under it do not jump from page to page. */}
          <div className="mt-5 grid">
            {pages.map((candidate) => {
              const active = candidate.id === page.id;
              const note = noteFor(candidate);
              return (
                <div
                  key={candidate.id}
                  aria-hidden={!active}
                  className={`[grid-area:1/1] ${
                    active ? "whats-new__text" : "invisible"
                  }`}
                >
                  <h3 className="whats-new__title text-xl font-semibold text-zinc-900 dark:text-white">
                    {t(`whatsNew.pages.${candidate.id}.title`)}
                  </h3>
                  <p className="whats-new__body mt-2 text-sm leading-relaxed text-zinc-600 dark:text-zinc-300">
                    {t(`whatsNew.pages.${candidate.id}.body`)}
                  </p>
                  {note && (
                    <p className="whats-new__note mt-2 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400">
                      {note}
                    </p>
                  )}
                </div>
              );
            })}
          </div>
          <button
            type="button"
            onClick={() => void run(page)}
            className="whats-new__primary mt-4 inline-flex items-center gap-2 rounded-full bg-emerald-500 px-5 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-emerald-600"
          >
            {needs === "library"
              ? t("whatsNew.addMusic")
              : needs === "track"
                ? t("whatsNew.openLibrary")
                : t("whatsNew.try")}
            <ArrowRight size={16} className="rtl:rotate-180" />
          </button>
        </div>

        <div className="mt-4 flex shrink-0 items-center gap-3 px-6">
          <button
            type="button"
            onClick={() => goTo(index - 1)}
            disabled={isFirst}
            className="whats-new__ghost inline-flex items-center gap-1 rounded-full px-3 py-2 text-sm text-zinc-600 transition-colors hover:bg-zinc-100 disabled:cursor-default disabled:opacity-40 disabled:hover:bg-transparent dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            <ChevronLeft size={16} className="rtl:rotate-180" />
            {t("whatsNew.previous")}
          </button>
          <div className="flex flex-1 flex-wrap items-center justify-center gap-1.5">
            {pages.map((candidate, i) => (
              <button
                key={candidate.id}
                type="button"
                onClick={() => goTo(i)}
                aria-label={t("whatsNew.goToPage", { page: i + 1 })}
                aria-current={i === index ? "true" : undefined}
                data-on={i === index || undefined}
                className="whats-new__dot h-2 w-2 rounded-full bg-zinc-300 transition-colors dark:bg-zinc-700"
              />
            ))}
          </div>
          <button
            ref={nextRef}
            type="button"
            data-next
            onClick={() => (isLast ? onClose() : goTo(index + 1))}
            className="whats-new__ghost inline-flex items-center gap-1 rounded-full px-3 py-2 text-sm font-medium text-zinc-800 transition-colors hover:bg-zinc-100 dark:text-zinc-100 dark:hover:bg-zinc-800"
          >
            {isLast ? t("whatsNew.done") : t("whatsNew.next")}
            {!isLast && <ChevronRight size={16} className="rtl:rotate-180" />}
          </button>
        </div>

        <div className="flex shrink-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 px-6 pb-4 pt-2">
          <label className="whats-new__option inline-flex cursor-pointer items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400">
            <input
              type="checkbox"
              checked={!autoOpen.value}
              disabled={!autoOpen.ready}
              onChange={(event) =>
                void autoOpen.setValue(!event.target.checked)
              }
              className="whats-new__check h-3.5 w-3.5"
            />
            {t("whatsNew.dontShow")}
          </label>
          <button
            type="button"
            onClick={() => {
              onClose();
              openView({ view: "about", aboutChangelog: true });
            }}
            className="whats-new__link text-xs text-zinc-500 underline-offset-2 transition-colors hover:text-zinc-800 hover:underline dark:text-zinc-400 dark:hover:text-zinc-100"
          >
            {t("whatsNew.history")}
          </button>
        </div>
      </AnimatedModalContent>
    </AnimatedModalShell>
  );
}
