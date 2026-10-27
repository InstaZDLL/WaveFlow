import { useTranslation } from "react-i18next";
import { useEffect, useRef, useState } from "react";
import {
  SUPPORTED_LANGUAGES,
  normalizeSupportedLanguageCode,
} from "../../../i18n";
import { Check, ChevronDown } from "lucide-react";
import { createPortal } from "react-dom";

interface LanguageDropdownProps {
  currentCode: string;
  onSelect: (code: string) => void;
}

export function LanguageDropdown({
  currentCode,
  onSelect,
}: LanguageDropdownProps) {
  const { t } = useTranslation();
  const [isOpen, setIsOpen] = useState(false);
  const [focusedIndex, setFocusedIndex] = useState(0);
  // Viewport-relative coordinates for the portaled listbox. Captured
  // once per open from the trigger's bounding rect; closed on scroll
  // or resize so a stale position never paints.
  const [position, setPosition] = useState<{
    top: number;
    right: number;
  } | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listboxRef = useRef<HTMLUListElement>(null);
  const optionRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const normalizedCurrentCode = normalizeSupportedLanguageCode(currentCode);

  const currentLanguage =
    SUPPORTED_LANGUAGES.find((lang) => lang.code === normalizedCurrentCode) ??
    SUPPORTED_LANGUAGES[0];

  // Click-outside + Escape handling. The listbox is portaled to
  // document.body to escape the parent stacking context (the Lounge +
  // Liquid skins paint Settings cards through `backdrop-filter`, which
  // by spec creates a new stacking context and traps `z-index` of any
  // descendant — so the dropdown rendered as an absolute child landed
  // visually behind the cards below it). Click-outside therefore has
  // to whitelist the portaled listbox too, otherwise clicking an
  // option dismisses the menu before its onClick handler runs.
  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      const target = event.target as Node;
      const inTrigger = containerRef.current?.contains(target);
      const inListbox = listboxRef.current?.contains(target);
      if (!inTrigger && !inListbox) {
        setIsOpen(false);
      }
    };

    const handleKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setIsOpen(false);
        // Return focus to the trigger so keyboard users stay anchored
        // to where they opened the listbox from. Matches WAI-ARIA APG
        // for combobox/listbox dismissal via Escape.
        triggerRef.current?.focus();
      }
    };

    document.addEventListener("mousedown", handleClickOutside);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKey);
    };
  }, [isOpen]);

  // Close on scroll/resize — the listbox uses fixed positioning derived
  // from a snapshot of the trigger's rect, so any layout shift would
  // leave it visually detached from the trigger. Closing is simpler
  // (and matches native <select>) than re-measuring on every frame.
  useEffect(() => {
    if (!isOpen) return;
    const close = () => setIsOpen(false);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [isOpen]);

  // Keep keyboard focus on the highlighted option when it changes
  useEffect(() => {
    if (isOpen) {
      optionRefs.current[focusedIndex]?.focus();
    }
  }, [isOpen, focusedIndex]);

  const handleTriggerClick = () => {
    setIsOpen((prev) => {
      if (!prev) {
        // Opening: snapshot the trigger's viewport rect for the
        // portaled listbox, then focus the currently selected option.
        // `mt-2` (= 8px) keeps the visual gap the old absolute markup
        // had; `right` is measured from the viewport edge to mirror
        // the original `right-0` anchor.
        const rect = triggerRef.current?.getBoundingClientRect();
        if (rect) {
          setPosition({
            top: rect.bottom + 8,
            right: window.innerWidth - rect.right,
          });
        }
        const initialIndex = Math.max(
          0,
          SUPPORTED_LANGUAGES.findIndex(
            (lang) => lang.code === normalizedCurrentCode,
          ),
        );
        setFocusedIndex(initialIndex);
      }
      return !prev;
    });
  };

  const handleOptionKeyDown = (
    event: React.KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setFocusedIndex((index + 1) % SUPPORTED_LANGUAGES.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setFocusedIndex(
        (index - 1 + SUPPORTED_LANGUAGES.length) % SUPPORTED_LANGUAGES.length,
      );
    } else if (event.key === "Home") {
      event.preventDefault();
      setFocusedIndex(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setFocusedIndex(SUPPORTED_LANGUAGES.length - 1);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      handleSelect(SUPPORTED_LANGUAGES[index].code);
    } else if (event.key === "Tab") {
      // Dismiss the listbox and let the browser's default Tab action
      // continue from the trigger's position in tab order. Focus is
      // moved synchronously to the trigger before this handler
      // returns; the un-prevented Tab default then advances from
      // there, so Tab lands on the next focusable element AFTER the
      // trigger and Shift+Tab lands on the previous one. Without this
      // shim, Tab from a portaled option escapes through document.body
      // and the keyboard caret falls off the end of tab order.
      setIsOpen(false);
      triggerRef.current?.focus();
    }
  };

  const handleSelect = (code: string) => {
    onSelect(code);
    setIsOpen(false);
    // Same restoration as the Escape branch: a deliberate option pick
    // (mouse click or Enter/Space) lands the keyboard caret back on
    // the trigger so the user can keep tabbing forward from where
    // they were. Click-outside is intentionally NOT mirrored here —
    // there the user explicitly aimed at something else, and stealing
    // focus back to the trigger would override that intent.
    triggerRef.current?.focus();
  };

  return (
    <div className="relative" ref={containerRef}>
      <button
        ref={triggerRef}
        type="button"
        onClick={handleTriggerClick}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
        aria-label={t("settings.language.title")}
        className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500"
      >
        <span>{currentLanguage.nativeLabel}</span>
        <ChevronDown
          size={14}
          className={`transition-transform ${isOpen ? "rotate-180" : ""}`}
        />
      </button>

      {isOpen &&
        position &&
        createPortal(
          <ul
            ref={listboxRef}
            role="listbox"
            aria-label={t("settings.language.title")}
            style={{
              position: "fixed",
              top: position.top,
              right: position.right,
            }}
            className="min-w-48 rounded-xl border border-zinc-200 bg-white shadow-lg dark:border-zinc-700 dark:bg-surface-dark-elevated dark:shadow-black/40 overflow-hidden z-[100] animate-fade-in py-1"
          >
            {SUPPORTED_LANGUAGES.map((lang, index) => {
              const isSelected = lang.code === normalizedCurrentCode;
              return (
                <li key={lang.code} role="presentation">
                  <button
                    ref={(el) => {
                      optionRefs.current[index] = el;
                    }}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    onClick={() => handleSelect(lang.code)}
                    onKeyDown={(event) => handleOptionKeyDown(event, index)}
                    className={`w-full flex items-center justify-between px-4 py-2 text-sm text-left transition-colors focus:outline-none ${
                      isSelected
                        ? "bg-emerald-50 text-emerald-700 dark:bg-emerald-900/20 dark:text-emerald-400"
                        : "text-zinc-700 dark:text-zinc-300 hover:bg-zinc-50 dark:hover:bg-zinc-700/30 focus:bg-zinc-50 dark:focus:bg-zinc-700/30"
                    }`}
                  >
                    <span>{lang.nativeLabel}</span>
                    {isSelected && <Check size={14} />}
                  </button>
                </li>
              );
            })}
          </ul>,
          document.body,
        )}
    </div>
  );
}
