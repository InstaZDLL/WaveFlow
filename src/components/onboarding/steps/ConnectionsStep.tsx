import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  CheckCircle2,
  ExternalLink,
  Eye,
  EyeOff,
  Loader2,
  Settings2,
} from "lucide-react";
import { openUrl } from "../../../lib/tauri/external";
import {
  getDiscordRpcEnabled,
  getLastfmApiKey,
  getLastfmApiSecret,
  lastfmGetStatus,
  lastfmLogin,
  setDiscordRpcEnabled,
  setLastfmApiKey,
  setLastfmApiSecret,
  type LastfmStatus,
} from "../../../lib/tauri/integration";
import { SETTINGS } from "../../../lib/settingsRegistry";
import { getOfflineMode } from "../../../lib/tauri/offline";
import { useEnrichmentSwitch } from "../../../hooks/useEnrichmentSettings";
import { openView } from "../../../lib/discovery";
import { FeatureCard } from "../../discovery/FeatureCard";
import { StepHeading } from "./StepHeading";

/**
 * Connections: Last.fm (the sign-in Settings → Integrations has), the
 * Discord status (on unless turned off — shown so it can be), and the
 * WaveFlow server, whose setup lives in Settings.
 */
export function ConnectionsStep({
  onTouch,
  onLeave,
  storeUnreachable,
}: {
  onTouch: () => void;
  /** The server link ends the journey to open its setting. */
  onLeave: () => void;
  /** The plugin store could not be reached earlier in the journey: the
   *  network is probably down, and the cards that need it only present. */
  storeUnreachable: boolean;
}) {
  const { t } = useTranslation();

  const [discord, setDiscord] = useState<boolean | null>(null);
  useEffect(() => {
    let cancelled = false;
    getDiscordRpcEnabled()
      .then((on) => {
        if (!cancelled) setDiscord(on);
      })
      .catch((err) => {
        console.error("[Onboarding] read discord rpc failed", err);
        if (!cancelled) {
          setDiscord(SETTINGS["integrations.discord_rpc"].defaultValue);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const toggleDiscord = () => {
    if (discord == null) return;
    onTouch();
    const next = !discord;
    setDiscord(next);
    setDiscordRpcEnabled(next).catch((err) => {
      console.error("[Onboarding] set discord rpc failed", err);
      setDiscord(!next);
    });
  };

  return (
    <div>
      <StepHeading
        title={t("onboarding.connections.title")}
        lede={t("onboarding.connections.lede")}
      />
      <div className="mt-7 grid grid-cols-1 items-start gap-3 sm:grid-cols-2">
        <div className="grid min-w-0 grid-cols-1 gap-3">
          <LastfmCard onTouch={onTouch} />
          <EnrichmentCard
            onTouch={onTouch}
            storeUnreachable={storeUnreachable}
          />
        </div>
        <div className="grid min-w-0 grid-cols-1 gap-3">
          <FeatureCard
            art="discord"
            title={t("onboarding.connections.discord.title")}
            benefit={t("onboarding.connections.discord.benefit")}
            checked={discord ?? false}
            disabled={discord == null}
            onToggle={toggleDiscord}
          />
          <FeatureCard
            art="server"
            title={t("onboarding.connections.server.title")}
            benefit={t("onboarding.connections.server.benefit")}
            size="compact"
          >
            <button
              type="button"
              onClick={() => {
                openView({
                  view: "settings",
                  settingsCategory: "integrations",
                });
                onLeave();
              }}
              className="onboarding-ghost mt-3 inline-flex items-center gap-1.5 self-start rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
            >
              <Settings2 size={13} aria-hidden="true" />
              {t("onboarding.connections.server.open")}
            </button>
          </FeatureCard>
        </div>
      </div>
    </div>
  );
}

/**
 * MusicBrainz enrichment, the switch of Settings → Library through the
 * same hook (`useEnrichmentSwitch`): the setting, then a pass started at
 * once. Off by default. Offline — the app's offline mode, or a store that
 * could not be reached — the card only presents, as the plugins do: a
 * pass would have nothing to ask.
 */
function EnrichmentCard({
  onTouch,
  storeUnreachable,
}: {
  onTouch: () => void;
  storeUnreachable: boolean;
}) {
  const { t } = useTranslation();
  const engine = useEnrichmentSwitch();
  const [offlineMode, setOfflineMode] = useState(false);
  useEffect(() => {
    let cancelled = false;
    getOfflineMode()
      .then((on) => {
        if (!cancelled) setOfflineMode(on);
      })
      .catch((err) => console.error("[Onboarding] read offline failed", err));
    return () => {
      cancelled = true;
    };
  }, []);

  const title = t("settings.enrichment.title");
  const benefit = t("settings.enrichment.subtitle");
  if ((offlineMode || storeUnreachable) && !engine.enabled) {
    return (
      <FeatureCard art="enrichment" title={title} benefit={benefit}>
        <p className="mt-3 flex items-center gap-1.5 text-xs text-zinc-500 dark:text-zinc-400">
          <Settings2 size={13} aria-hidden="true" className="shrink-0" />
          {t("onboarding.connections.enrichmentOffline")}
        </p>
      </FeatureCard>
    );
  }
  return (
    <FeatureCard
      art="enrichment"
      title={title}
      benefit={benefit}
      checked={engine.enabled}
      disabled={!engine.ready}
      onToggle={() => {
        onTouch();
        void engine.toggle();
      }}
      status={
        engine.error ? (
          <span className="text-rose-500" role="alert">
            {engine.error}
          </span>
        ) : undefined
      }
    />
  );
}

/**
 * The Last.fm sign-in, as Settings → Integrations has it: the API key and
 * secret are saved first, then a session is made from the user's name and
 * password, which is never kept.
 */
function LastfmCard({ onTouch }: { onTouch: () => void }) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<LastfmStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [showSecret, setShowSecret] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [s, k, sec] = await Promise.all([
          lastfmGetStatus(),
          getLastfmApiKey(),
          getLastfmApiSecret(),
        ]);
        if (cancelled) return;
        setStatus(s);
        if (k) setKey(k);
        if (sec) setSecret(sec);
      } catch (err) {
        console.error("[Onboarding] read lastfm failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const connect = useCallback(async () => {
    const k = key.trim();
    const s = secret.trim();
    const u = user.trim();
    if (!k || !s || !u || !password) {
      setError(t("onboarding.connections.lastfm.missingFields"));
      return;
    }
    onTouch();
    setBusy(true);
    setError(null);
    try {
      await setLastfmApiKey(k);
      await setLastfmApiSecret(s);
      setStatus(await lastfmLogin(u, password));
      setPassword("");
      setOpen(false);
    } catch (err) {
      console.error("[Onboarding] lastfm login failed", err);
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [key, secret, user, password, onTouch, t]);

  const connected = status?.connected ?? false;

  return (
    <FeatureCard
      art="lastfm"
      title={t("onboarding.connections.lastfm.title")}
      benefit={t("onboarding.connections.lastfm.benefit")}
      checked={connected}
    >
      {connected ? (
        <p className="mt-3 flex items-center gap-1.5 text-xs font-semibold text-emerald-600 dark:text-emerald-400">
          <CheckCircle2 size={14} aria-hidden="true" />
          {t("onboarding.connections.lastfm.connected", {
            user: status?.username ?? "",
          })}
        </p>
      ) : !open ? (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="onboarding-ghost mt-3 self-start rounded-full border border-zinc-200 px-3 py-1.5 text-xs font-semibold text-zinc-700 transition-colors hover:border-emerald-500 hover:text-emerald-600 dark:border-zinc-700 dark:text-zinc-200"
        >
          {t("onboarding.connections.lastfm.connect")}
        </button>
      ) : (
        <div className="mt-3 space-y-2">
          <button
            type="button"
            onClick={() =>
              void openUrl("https://www.last.fm/api/account/create").catch(
                (err) => console.error("[Onboarding] open url failed", err),
              )
            }
            className="inline-flex items-center gap-1 text-[11px] font-medium text-emerald-600 hover:underline dark:text-emerald-400"
          >
            {t("onboarding.connections.lastfm.keyHint")}
            <ExternalLink size={11} aria-hidden="true" />
          </button>
          <Field
            label={t("onboarding.connections.lastfm.key")}
            value={key}
            onChange={setKey}
          />
          <Field
            label={t("onboarding.connections.lastfm.secret")}
            value={secret}
            onChange={setSecret}
            secret={!showSecret}
            onReveal={() => setShowSecret((v) => !v)}
            revealLabel={t("onboarding.connections.lastfm.toggleSecret")}
          />
          <Field
            label={t("onboarding.connections.lastfm.user")}
            value={user}
            onChange={setUser}
          />
          <Field
            label={t("onboarding.connections.lastfm.password")}
            value={password}
            onChange={setPassword}
            secret={!showPassword}
            onReveal={() => setShowPassword((v) => !v)}
            revealLabel={t("onboarding.connections.lastfm.togglePassword")}
          />
          {error && (
            <p className="text-[11px] text-rose-500" role="alert">
              {error}
            </p>
          )}
          <button
            type="button"
            onClick={() => void connect()}
            disabled={busy}
            className="onboarding-primary inline-flex w-full items-center justify-center gap-2 rounded-full bg-emerald-500 px-4 py-2 text-xs font-semibold text-white transition-colors hover:bg-emerald-600 disabled:opacity-50"
          >
            {busy && <Loader2 size={13} className="animate-spin" />}
            {t("onboarding.connections.lastfm.signIn")}
          </button>
        </div>
      )}
    </FeatureCard>
  );
}

function Field({
  label,
  value,
  onChange,
  secret,
  onReveal,
  revealLabel,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  secret?: boolean;
  onReveal?: () => void;
  revealLabel?: string;
}) {
  return (
    <label className="block">
      <span className="mb-1 block text-[10px] font-bold uppercase tracking-widest text-zinc-500 dark:text-zinc-400">
        {label}
      </span>
      <span className="relative block">
        <input
          type={secret ? "password" : "text"}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          autoComplete="off"
          spellCheck={false}
          className={`w-full rounded-lg border border-zinc-200 bg-white px-3 py-1.5 text-xs text-zinc-900 focus:border-emerald-500/40 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 dark:border-zinc-700 dark:bg-zinc-900 dark:text-zinc-100 ${
            onReveal ? "pr-9" : ""
          }`}
        />
        {onReveal && (
          <button
            type="button"
            onClick={onReveal}
            aria-label={revealLabel}
            className="absolute inset-y-0 right-2 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
          >
            {secret ? <Eye size={13} /> : <EyeOff size={13} />}
          </button>
        )}
      </span>
    </label>
  );
}
