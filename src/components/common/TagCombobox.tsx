import { useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronDown } from "lucide-react";

/** A titled group of suggestions, shown in order. */
export interface SuggestionGroup {
  label: string;
  values: readonly string[];
  /**
   * Already matched against the input by whoever supplied them (a search
   * on the backend), so shown as they come. Filtering them again here
   * would drop some: the backend compares a normalised form in which
   * "ac dc" finds "AC/DC", and this list would not.
   */
  matched?: boolean;
}

interface TagComboboxProps {
  value: string;
  onChange: (value: string) => void;
  groups: readonly SuggestionGroup[];
  placeholder?: string;
  ariaLabel: string;
  disabled?: boolean;
  /** Classes of the text input; the list is sized to match it. */
  inputClassName: string;
}

/** Suggestions shown per group. A library can hold thousands of artists,
 *  and the list is for picking one, not for browsing them all — typing
 *  narrows it. */
const MAX_PER_GROUP = 80;

/** Case- and accent-insensitive form, so "beyonce" finds "Beyoncé". */
function fold(s: string): string {
  return s
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLocaleLowerCase();
}

/**
 * Text field with a list of suggestions, the way a tag editor offers the
 * genres it knows: click or type and the list opens, filtered as you go,
 * and anything typed is still accepted as is.
 *
 * The list is portalled and positioned against the input. The dialog it
 * lives in scrolls, and its sections clip their rounded corners with
 * `overflow: hidden`, which would cut an absolutely positioned list in
 * half. Focus never leaves the input: the options are pointed at with
 * `aria-activedescendant`, and a click on one is taken on mouse-down so
 * the input keeps focus — which also keeps the dialog's focus trap and
 * its backdrop out of it.
 */
export function TagCombobox({
  value,
  onChange,
  groups,
  placeholder,
  ariaLabel,
  disabled = false,
  inputClassName,
}: TagComboboxProps) {
  const listId = useId();
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [rect, setRect] = useState<{
    left: number;
    width: number;
    top?: number;
    bottom?: number;
    maxHeight: number;
  } | null>(null);

  const query = fold(value.trim());

  // Filtered groups, then flattened so the arrow keys walk through every
  // group as one list. Starts-with matches come before the rest, and a
  // value offered by an earlier group is not offered again by a later one
  // (a genre in the library and in the presets shows once, as the
  // library's).
  const { shown, flat } = useMemo(() => {
    const taken = new Set<string>();
    // `start`: where the group's first value sits in `flat`, which is
    // what an option's id and the highlighted index are counted in.
    const shown: (SuggestionGroup & { start: number })[] = [];
    const flat: string[] = [];
    for (const g of groups) {
      const starts: string[] = [];
      const contains: string[] = [];
      for (const v of g.values) {
        const f = fold(v);
        if (taken.has(f)) continue;
        if (g.matched || query === "" || f.startsWith(query)) starts.push(v);
        else if (f.includes(query)) contains.push(v);
      }
      for (const v of g.values) taken.add(fold(v));
      const values = starts.concat(contains).slice(0, MAX_PER_GROUP);
      if (values.length === 0) continue;
      shown.push({ label: g.label, values, start: flat.length });
      flat.push(...values);
    }
    return { shown, flat };
  }, [groups, query]);

  const visible = open && flat.length > 0 && !disabled;

  // Placed against the input, below it or — when the dialog leaves no
  // room there — above it. Re-measured on any scroll (captured, so the
  // dialog's own scroll counts) and on resize.
  useLayoutEffect(() => {
    if (!visible) return;
    const place = () => {
      const el = inputRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const margin = 8;
      const below = window.innerHeight - r.bottom - margin;
      const above = r.top - margin;
      const wanted = 256;
      if (below >= Math.min(wanted, above)) {
        setRect({
          left: r.left,
          width: r.width,
          top: r.bottom + 4,
          maxHeight: Math.min(wanted, below - 4),
        });
      } else {
        setRect({
          left: r.left,
          width: r.width,
          bottom: window.innerHeight - r.top + 4,
          maxHeight: Math.min(wanted, above - 4),
        });
      }
    };
    place();
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [visible]);

  // Keep the highlighted option in view while the arrow keys move it.
  useLayoutEffect(() => {
    if (!visible || active < 0) return;
    document
      .getElementById(`${listId}-${active}`)
      ?.scrollIntoView({ block: "nearest" });
  }, [visible, active, listId]);

  const pick = (choice: string) => {
    onChange(choice);
    setOpen(false);
    setActive(-1);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (flat.length === 0) return;
      e.preventDefault();
      if (!open) {
        setOpen(true);
        setActive(e.key === "ArrowDown" ? 0 : flat.length - 1);
        return;
      }
      const step = e.key === "ArrowDown" ? 1 : -1;
      setActive((i) => (i + step + flat.length) % flat.length);
    } else if (e.key === "Enter") {
      if (visible && active >= 0 && active < flat.length) {
        e.preventDefault();
        pick(flat[active]);
      }
    } else if (e.key === "Escape") {
      // First Escape closes the list only; the dialog listens on the
      // document, so stopping it here keeps the dialog open.
      if (visible) {
        e.stopPropagation();
        setOpen(false);
        setActive(-1);
      }
    }
  };

  const list = visible && rect && (
    <div
      id={listId}
      role="listbox"
      aria-label={ariaLabel}
      style={{
        position: "fixed",
        left: rect.left,
        width: rect.width,
        top: rect.top,
        bottom: rect.bottom,
        maxHeight: rect.maxHeight,
      }}
      className="z-101 overflow-y-auto rounded-lg border border-zinc-200 bg-white py-1 text-sm shadow-xl dark:border-zinc-700 dark:bg-zinc-800"
      // Mouse-down, not click: the input would lose focus first, and the
      // blur would close the list before the click landed.
      onMouseDown={(e) => e.preventDefault()}
    >
      {shown.map((g) => (
        <div key={g.label} role="group" aria-label={g.label}>
          {shown.length > 1 && (
            <div className="px-3 pt-2 pb-1 text-[10px] font-bold uppercase tracking-widest text-zinc-400">
              {g.label}
            </div>
          )}
          {g.values.map((v, k) => {
            const i = g.start + k;
            return (
              <div
                key={v}
                id={`${listId}-${i}`}
                role="option"
                aria-selected={i === active}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(v);
                }}
                onMouseEnter={() => setActive(i)}
                className={`cursor-pointer truncate px-3 py-1.5 ${
                  i === active
                    ? "bg-emerald-500 text-white"
                    : "text-zinc-700 dark:text-zinc-200"
                }`}
              >
                {v}
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );

  return (
    <div className="relative flex-1 min-w-0">
      <input
        ref={inputRef}
        type="text"
        role="combobox"
        aria-label={ariaLabel}
        aria-expanded={visible}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={
          visible && active >= 0 ? `${listId}-${active}` : undefined
        }
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
          setActive(-1);
        }}
        onClick={() => setOpen(true)}
        onBlur={() => {
          setOpen(false);
          setActive(-1);
        }}
        onKeyDown={onKeyDown}
        className={`${inputClassName} pr-7`}
      />
      <ChevronDown
        size={14}
        aria-hidden="true"
        className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-zinc-400"
      />
      {list && createPortal(list, document.body)}
    </div>
  );
}
