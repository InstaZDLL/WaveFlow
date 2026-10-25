import {
  useRef,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { Check } from "lucide-react";

export interface SettingsChoice<T extends string | number> {
  value: T;
  label: ReactNode;
  /** One or two lines under the label. */
  hint?: ReactNode;
  /** A small drawing of what the choice looks like (`settingsPreviews`). */
  preview?: ReactNode;
  disabled?: boolean;
}

interface SettingsChoiceCardsProps<T extends string | number> {
  choices: ReadonlyArray<SettingsChoice<T>>;
  value: T | null;
  /** The click, when there was one (a theme change animates from it). */
  onChange: (value: T, event?: MouseEvent<HTMLButtonElement>) => void;
  /** The id of the element naming the group (the row's title). */
  labelledBy?: string;
  label?: string;
  /** Inert while the stored value is being read. */
  disabled?: boolean;
  /** A change is being applied: the cards keep the focus (a disabled
   *  button would drop it to the page) but ignore every choice. */
  busy?: boolean;
  /** Minimum width of a card before the grid wraps, in rem. */
  minWidth?: number;
  /** Short drawings (bars, a strip) instead of a 16:10 scene. */
  compact?: boolean;
}

/**
 * A setting's options as selectable cards: a preview, a label and a hint,
 * the chosen one marked by the accent's ring and a check.
 *
 * A radio group built from buttons, as the skin picker is: the arrow keys
 * (and Home / End) move the selection *and* the focus, and only the chosen
 * card sits in the tab order (roving tabindex). A choice applies as soon as
 * it is selected, which is what a radio does.
 *
 * Styled by its own classes in `settings.css` rather than by colour
 * utilities: the fluid materials restyle anything that looks like a
 * selected settings row by matching utility substrings, and a card that
 * carried them would be painted twice.
 */
export function SettingsChoiceCards<T extends string | number>({
  choices,
  value,
  onChange,
  labelledBy,
  label,
  disabled = false,
  busy = false,
  minWidth = 9,
  compact = false,
}: SettingsChoiceCardsProps<T>) {
  const refs = useRef<Array<HTMLButtonElement | null>>([]);
  const selectedIndex = choices.findIndex((c) => c.value === value);

  const move = (event: KeyboardEvent<HTMLButtonElement>, from: number) => {
    const len = choices.length;
    // In a right-to-left page the horizontal arrows mirror.
    const rtl =
      event.currentTarget.closest("[dir]")?.getAttribute("dir") === "rtl";
    // The cards to try, in order: the first enabled one wins.
    let order: number[];
    switch (event.key) {
      case "ArrowRight":
      case "ArrowLeft":
      case "ArrowDown":
      case "ArrowUp": {
        const forward =
          event.key === "ArrowDown" ||
          (event.key === "ArrowRight" && !rtl) ||
          (event.key === "ArrowLeft" && rtl);
        order = Array.from(
          { length: len - 1 },
          (_, i) => (from + (forward ? i + 1 : -(i + 1)) + len) % len,
        );
        break;
      }
      case "Home":
        order = Array.from({ length: len }, (_, i) => i);
        break;
      case "End":
        order = Array.from({ length: len }, (_, i) => len - 1 - i);
        break;
      default:
        return;
    }
    event.preventDefault();
    if (busy) return;
    const target = order.find((i) => !choices[i].disabled);
    if (target === undefined) return;
    onChange(choices[target].value);
    refs.current[target]?.focus();
  };

  return (
    <div
      role="radiogroup"
      aria-labelledby={labelledBy}
      aria-label={labelledBy ? undefined : label}
      aria-busy={disabled || busy || undefined}
      className={
        compact
          ? "settings-choices settings-choices--compact"
          : "settings-choices"
      }
      style={{ ["--settings-choice-min" as string]: `${minWidth}rem` }}
    >
      {choices.map((choice, index) => {
        const selected = choice.value === value;
        // With nothing selected yet, the first card takes the tab stop.
        const tabbable = selectedIndex === -1 ? index === 0 : selected;
        return (
          <button
            key={String(choice.value)}
            ref={(el) => {
              refs.current[index] = el;
            }}
            type="button"
            role="radio"
            aria-checked={selected}
            tabIndex={tabbable ? 0 : -1}
            disabled={disabled || choice.disabled}
            data-selected={selected || undefined}
            data-preview={choice.preview ? "" : undefined}
            aria-disabled={busy || undefined}
            onClick={(event) => {
              if (!selected && !busy) onChange(choice.value, event);
            }}
            onKeyDown={(event) => move(event, index)}
            className="settings-choice"
          >
            {choice.preview && (
              <span className="settings-choice-preview" aria-hidden="true">
                {choice.preview}
              </span>
            )}
            <span className="settings-choice-body">
              <span className="settings-choice-label">{choice.label}</span>
              {choice.hint && (
                <span className="settings-choice-hint">{choice.hint}</span>
              )}
            </span>
            <span className="settings-choice-check" aria-hidden="true">
              {selected && <Check size={12} strokeWidth={3} />}
            </span>
          </button>
        );
      })}
    </div>
  );
}
