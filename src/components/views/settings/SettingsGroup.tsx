import { useEffect, useRef, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";

import { findSettingsGroup, type SettingsGroupId } from "./settingsCatalog";

interface SettingsGroupProps {
  id: SettingsGroupId;
  /** Collapsed behind a disclosure until opened (or revealed). */
  advanced?: boolean;
  /** The child is a framed card of its own (the equaliser, the
   *  shortcuts): no second card around it. */
  bare?: boolean;
  reveal?: boolean;
  children: ReactNode;
}

/**
 * One titled block of a settings page: a small uppercase title with the
 * group's one-line hint beside it, over a card that holds the group's
 * rows, separated by hairlines (`settings.css`).
 *
 * The title and the hint come from the group's catalog entry, the one the
 * settings search reads, so a label changed there shows up here too.
 *
 * The card carries `rounded-2xl` and a border so the fluid materials give
 * it the surface they give every card; Studio paints it from the tokens
 * in `settings.css`.
 */
export function SettingsGroup({
  id,
  advanced,
  bare,
  reveal,
  children,
}: SettingsGroupProps) {
  const { t } = useTranslation();
  const detailsRef = useRef<HTMLDetailsElement>(null);

  useEffect(() => {
    if (reveal && detailsRef.current) detailsRef.current.open = true;
  }, [reveal]);

  const entry = findSettingsGroup(id);
  const title = entry ? t(entry.labelKey) : id;
  const hint = entry ? t(entry.hintKey) : null;

  const heading = (
    <div className="settings-group-heading">
      <h3 id={`settings-group-heading-${id}`} className="settings-group-title">
        {title}
      </h3>
      {hint && <p className="settings-group-hint">{hint}</p>}
    </div>
  );

  if (advanced) {
    return (
      <details
        ref={detailsRef}
        id={`settings-group-${id}`}
        tabIndex={-1}
        className="settings-group settings-card settings-card--disclosure group scroll-mt-6 rounded-2xl border focus-visible:outline-emerald-500"
      >
        <summary className="settings-disclosure flex cursor-pointer list-none items-center justify-between gap-4 rounded-2xl focus-visible:outline-emerald-500 [&::-webkit-details-marker]:hidden">
          {heading}
          <ChevronDown
            size={16}
            aria-hidden="true"
            className="settings-disclosure-chevron shrink-0 transition-transform group-open:rotate-180"
          />
        </summary>
        <div className="settings-group-body">{children}</div>
      </details>
    );
  }

  return (
    <section
      id={`settings-group-${id}`}
      aria-labelledby={`settings-group-heading-${id}`}
      tabIndex={-1}
      className="settings-group scroll-mt-6 focus-visible:outline-emerald-500"
    >
      {heading}
      {bare ? (
        children
      ) : (
        <div className="settings-group-body settings-card rounded-2xl border">
          {children}
        </div>
      )}
    </section>
  );
}
