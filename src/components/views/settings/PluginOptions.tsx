import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check } from "lucide-react";

import {
  getPluginOptions,
  setPluginOption,
  type PluginInfo,
  type PluginOption,
} from "../../../lib/tauri/plugins";
import { useLocalizedText } from "../../../hooks/useLocalizedText";

/**
 * Inline per-plugin options panel, revealed under a plugin row when its gear
 * is clicked (see PluginsCard). Keeps options scoped to the plugin the user
 * opened instead of stacking a card per plugin — the list stays short no
 * matter how many plugins are installed.
 *
 * Only what the plugin's own manifest declares. The motion-artwork and Canvas
 * cache controls used to render here too, keyed on the plugin's world, and
 * both keys were wrong for the same reason: those caches are app-wide, so the
 * same toggle, footprint and "Clear cache" appeared under every plugin of the
 * world — and a world does not say what a plugin produces. The first
 * `waveflow:metadata` plugin that ships lyrics rather than motion covers
 * showed a motion-cache switch that did nothing it could see. They live in
 * Settings → Data now, beside the cache location (`MediaCachesCard`).
 */
export function PluginOptions({ plugin }: { plugin: PluginInfo }) {
  return (
    <div className="mt-3 pt-3 border-t border-zinc-200 dark:border-zinc-800 space-y-4">
      <ManifestOptions pluginId={plugin.id} />
    </div>
  );
}

/**
 * Renders a plugin's manifest-declared `[[options]]` as controls (switch for
 * `bool`, dropdown for `enum`, text field for `text`). Labels come from the
 * manifest (plugin-authored), so they never go through `t()` — a manifest can
 * instead ship its own `{ lang: text }` map, resolved here against the active
 * language (see `useLocalizedText`). Each change is optimistic and reverts by
 * re-fetch on error; writes serialise per panel.
 */
function ManifestOptions({ pluginId }: { pluginId: string }) {
  const [options, setOptions] = useState<PluginOption[]>([]);
  const [loading, setLoading] = useState(true);
  const [savingKey, setSavingKey] = useState<string | null>(null);
  // The option just written, for the "Saved" confirmation. A text field
  // gave no sign of having been saved at all, so people pasted a token
  // and did not know whether to press Enter.
  const [savedKey, setSavedKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (savedKey == null) return;
    const timer = window.setTimeout(() => setSavedKey(null), 2500);
    return () => window.clearTimeout(timer);
  }, [savedKey]);

  useEffect(() => {
    let cancelled = false;
    getPluginOptions(pluginId).then(
      (opts) => {
        if (cancelled) return;
        setOptions(opts);
        setLoading(false);
      },
      (e) => {
        if (cancelled) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [pluginId]);

  const onChange = useCallback(
    async (key: string, value: string | null) => {
      if (savingKey) return;
      setSavingKey(key);
      setSavedKey(null);
      setError(null);
      setOptions((prev) =>
        prev.map((o) =>
          o.key === key
            ? {
                ...o,
                // A sensitive value is never held here, only whether one is.
                value: o.sensitive ? null : value,
                isSet: value != null && value !== "",
              }
            : o,
        ),
      ); // optimistic
      try {
        await setPluginOption(pluginId, key, value);
        setSavedKey(key);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        // Revert to the persisted truth.
        const fresh = await getPluginOptions(pluginId).catch(() => null);
        if (fresh) setOptions(fresh);
      } finally {
        setSavingKey(null);
      }
    },
    [pluginId, savingKey],
  );

  if (loading || options.length === 0) return null;

  return (
    <div className="space-y-3">
      {error && (
        <div
          role="alert"
          className="px-2 py-1.5 bg-red-50 dark:bg-red-950/30 text-xs text-red-700 dark:text-red-300 rounded"
        >
          {error}
        </div>
      )}
      {options.map((option) => (
        <OptionControl
          key={option.key}
          option={option}
          disabled={savingKey !== null}
          saved={savedKey === option.key}
          onChange={(v) => onChange(option.key, v)}
        />
      ))}
    </div>
  );
}

function OptionControl({
  option,
  disabled,
  saved,
  onChange,
}: {
  option: PluginOption;
  disabled: boolean;
  saved: boolean;
  onChange: (value: string | null) => void;
}) {
  const { t } = useTranslation();
  const localized = useLocalizedText();
  // Effective value = user override, else the manifest default.
  const effective = option.value ?? option.default ?? "";
  // `key` is the last-resort label: a manifest whose label map has no
  // usable entry must still leave the control with an accessible name.
  const label = localized(option.label) ?? option.key;
  const description = localized(option.description);

  return (
    <div className="flex items-center justify-between gap-4">
      <div className="min-w-0">
        <div className="text-sm text-zinc-700 dark:text-zinc-200">{label}</div>
        {description && (
          <p className="text-xs text-zinc-500 dark:text-zinc-400 mt-0.5">
            {description}
          </p>
        )}
      </div>
      {option.type === "bool" ? (
        <button
          type="button"
          role="switch"
          aria-checked={effective === "true"}
          aria-label={label}
          disabled={disabled}
          onClick={() => onChange(effective === "true" ? "false" : "true")}
          className={`relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 focus-visible:ring-offset-2 disabled:opacity-50 ${
            effective === "true"
              ? "bg-emerald-500"
              : "bg-zinc-300 dark:bg-zinc-700"
          }`}
        >
          <span
            className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
              effective === "true" ? "translate-x-6" : "translate-x-1"
            }`}
          />
        </button>
      ) : option.type === "enum" ? (
        <select
          value={effective}
          disabled={disabled}
          aria-label={label}
          onChange={(e) => onChange(e.target.value)}
          className="shrink-0 text-sm rounded-md border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200 px-2 py-1 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
        >
          {option.choices.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
      ) : (
        <TextOption
          // Remounted whenever the stored value moves — a revert after a
          // failed save must show — which also leaves a sensitive field
          // blank again after each write.
          key={`${effective}:${option.isSet}`}
          option={option}
          effective={effective}
          label={label}
          disabled={disabled}
          saved={saved}
          onChange={onChange}
          savedLabel={t("settings.plugins.options.saved")}
          storedPlaceholder={t("settings.plugins.options.secretStored")}
          clearLabel={t("settings.plugins.options.secretClear")}
        />
      )}
    </div>
  );
}

/**
 * A `text` option. Saves on Enter and on leaving the field — a credential
 * also the moment it is pasted — and says so: it used to save on blur
 * alone, silently.
 *
 * A sensitive option (a cookie, a token) is a password field that starts
 * empty: the host never sends the stored value back, only whether there
 * is one, which the placeholder says. Pasting replaces it; the clear
 * button removes it.
 */
function TextOption({
  option,
  effective,
  label,
  disabled,
  saved,
  onChange,
  savedLabel,
  storedPlaceholder,
  clearLabel,
}: {
  option: PluginOption;
  effective: string;
  label: string;
  disabled: boolean;
  saved: boolean;
  onChange: (value: string | null) => void;
  savedLabel: string;
  storedPlaceholder: string;
  clearLabel: string;
}) {
  const sensitive = option.sensitive;
  const [draft, setDraft] = useState(sensitive ? "" : effective);

  const commit = (value: string) => {
    const next = value.trim();
    if (sensitive) {
      if (next === "") return;
      onChange(next);
      setDraft("");
      return;
    }
    if (next !== effective) onChange(next);
  };

  return (
    <div className="shrink-0 flex items-center gap-2">
      {saved && (
        <span
          role="status"
          className="inline-flex items-center gap-1 text-xs text-emerald-600 dark:text-emerald-400"
        >
          <Check size={12} aria-hidden="true" />
          {savedLabel}
        </span>
      )}
      <input
        type={sensitive ? "password" : "text"}
        value={draft}
        disabled={disabled}
        aria-label={label}
        autoComplete="off"
        spellCheck={false}
        placeholder={sensitive && option.isSet ? storedPlaceholder : undefined}
        onChange={(e) => setDraft(e.target.value)}
        onPaste={(e) => {
          // A credential is saved the moment it is pasted: pasting it is
          // the whole gesture, and nothing on screen said Enter was
          // needed. A plain text option keeps ordinary editing — a paste
          // may land in the middle of what is there.
          if (!sensitive) return;
          const pasted = e.clipboardData.getData("text");
          if (!pasted) return;
          e.preventDefault();
          // What the field would hold after the paste, selection included:
          // normally the whole (empty) field, but not always.
          const input = e.currentTarget;
          const start = input.selectionStart ?? draft.length;
          const end = input.selectionEnd ?? draft.length;
          commit(draft.slice(0, start) + pasted + draft.slice(end));
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit(draft);
        }}
        onBlur={() => commit(draft)}
        className="w-40 text-sm rounded-md border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200 px-2 py-1 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50"
      />
      {sensitive && option.isSet && (
        <button
          type="button"
          disabled={disabled}
          onClick={() => onChange(null)}
          className="text-xs text-zinc-500 hover:text-zinc-800 dark:hover:text-white underline-offset-2 hover:underline disabled:opacity-50"
        >
          {clearLabel}
        </button>
      )}
    </div>
  );
}
