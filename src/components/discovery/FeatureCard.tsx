import { useId, type ReactNode } from "react";
import { FeatureArt, type FeatureArtKind } from "./FeatureArt";
import "./discovery.css";

export interface FeatureCardProps {
  art: FeatureArtKind;
  title: string;
  /** What it does for the listener, in one sentence. */
  benefit: string;
  /**
   * A card that is a switch: the whole card toggles it, and `checked`
   * draws the state. Leave both out for a card that only presents.
   * The switch is a button laid over the card and not around it: what a
   * button holds is presentational, so a `status` inside it would never
   * be announced.
   */
  checked?: boolean;
  onToggle?: () => void;
  disabled?: boolean;
  /** Short facts in the card's corner ("Word by word", "Recommended"). */
  badges?: string[];
  /** A state line for a card that is not a switch ("Installed"). */
  status?: ReactNode;
  /** Actions or rows under the text. Never with `onToggle`: the card
   *  is then a button, and a button holds no other control. */
  children?: ReactNode;
  /** `compact` drops the scene to a strip, for a list of many. */
  size?: "default" | "compact";
}

/**
 * One feature told in a card: a small animated scene, a title, the
 * benefit in a sentence and its state. Activated in a click when it is a
 * switch. Shared by the onboarding and the Home's first steps: nothing in
 * it knows which screen it is on. The "What's new" panel tells a feature at
 * another scale, through `FeatureHero`, with the same scenes.
 */
export function FeatureCard({
  art,
  title,
  benefit,
  checked,
  onToggle,
  disabled,
  badges,
  status,
  children,
  size = "default",
}: FeatureCardProps) {
  const isSwitch = onToggle != null;
  const titleId = useId();
  const benefitId = useId();
  const body = (
    <>
      <div
        className={`feature-card__art rounded-[0.625rem] bg-zinc-100 text-zinc-500 dark:bg-white/5 dark:text-zinc-400 ${
          size === "compact" ? "h-16" : "h-24"
        }`}
      >
        <FeatureArt kind={art} />
      </div>
      <div className="mt-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-1.5">
            <span
              id={titleId}
              className="feature-card__title text-sm font-semibold text-zinc-900 dark:text-white"
            >
              {title}
            </span>
            {badges?.map((badge) => (
              <span
                key={badge}
                className="rounded-full bg-emerald-500/12 px-1.5 py-px text-[10px] font-semibold uppercase tracking-wider text-emerald-700 dark:text-emerald-300"
              >
                {badge}
              </span>
            ))}
          </div>
          <p
            id={benefitId}
            className="feature-card__benefit mt-1 text-xs leading-relaxed text-zinc-500 dark:text-zinc-400"
          >
            {benefit}
          </p>
        </div>
        {isSwitch && (
          <span
            aria-hidden="true"
            className="feature-switch mt-0.5"
            data-on={checked ? "true" : "false"}
          />
        )}
      </div>
      {status != null && <div className="mt-2 text-xs">{status}</div>}
    </>
  );

  const frame =
    "feature-card relative flex flex-col rounded-2xl border border-zinc-200 bg-white p-3 text-left dark:border-zinc-800 dark:bg-zinc-900";

  if (isSwitch) {
    return (
      <div
        data-checked={checked ? "true" : "false"}
        data-switch={disabled ? "disabled" : "enabled"}
        className={`${frame} transition-colors ${
          disabled
            ? "opacity-60"
            : "hover:border-zinc-300 dark:hover:border-zinc-700"
        }`}
      >
        {body}
        <button
          type="button"
          role="switch"
          aria-checked={!!checked}
          aria-labelledby={titleId}
          aria-describedby={benefitId}
          disabled={disabled}
          onClick={onToggle}
          className="absolute inset-0 rounded-[inherit] bg-transparent focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:cursor-wait"
        />
      </div>
    );
  }
  return (
    <div className={frame} data-checked={checked ? "true" : undefined}>
      {body}
      {children}
    </div>
  );
}
