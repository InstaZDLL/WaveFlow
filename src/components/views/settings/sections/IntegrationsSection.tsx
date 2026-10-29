import type { Dispatch, RefObject, SetStateAction } from "react";
import type { Profile } from "../../../../lib/tauri/profile";
import type { DlnaConfig, DlnaStatus } from "../../../../lib/tauri/dlna";
import type { LastfmStatus } from "../../../../lib/tauri/integration";
import type { MpdConfig, MpdStatus } from "../../../../lib/tauri/mpd";
import { useTranslation } from "react-i18next";
import { CatalogueMirrorCard } from "../CatalogueMirrorCard";
import { ListenBrainzCard } from "../ListenBrainzCard";
import { ReconciliationCard } from "../ReconciliationCard";
import { RemoteServerCard } from "../RemoteServerCard";
import { RemoteTranscodeCard } from "../RemoteTranscodeCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ToggleSwitch } from "../ToggleSwitch";
import { UploadToServerCard } from "../UploadToServerCard";
import {
  Check as CheckIcon,
  Copy,
  Eye,
  EyeOff,
  Gamepad2,
  Network,
  Radio,
  Server,
  WifiOff,
} from "lucide-react";

interface IntegrationsSectionProps {
  activeProfile: Profile | null;
  discordRpc: boolean;
  dlnaConfig: DlnaConfig;
  dlnaStatus: DlnaStatus | null;
  dlnaUrlCopied: boolean;
  handleDlnaCopyUrl: () => void;
  handleGenerateMpdPassword: () => Promise<void>;
  handleLastfmLogin: () => Promise<void>;
  handleLastfmLogout: () => Promise<void>;
  handleMpdCopyAddress: () => void;
  handleSaveLastfmKey: () => Promise<void>;
  handleToggleDiscordRpc: () => void;
  handleToggleDlna: () => void;
  handleToggleMpd: () => void;
  handleToggleOfflineMode: () => void;
  lastfmKey: string;
  lastfmKeyVisible: boolean;
  lastfmLoggingIn: boolean;
  lastfmLoginError: string | null;
  lastfmPassword: string;
  lastfmSaved: boolean;
  lastfmSaving: boolean;
  lastfmSecret: string;
  lastfmSecretVisible: boolean;
  lastfmStatus: LastfmStatus | null;
  lastfmUsername: string;
  mpdAddressCopied: boolean;
  mpdConfig: MpdConfig;
  mpdConfirmedRef: RefObject<MpdConfig | null>;
  mpdGenerating: boolean;
  mpdHydrated: boolean;
  mpdPasswordGenerated: boolean;
  mpdPasswordInputRef: RefObject<HTMLInputElement | null>;
  mpdPasswordVisible: boolean;
  mpdSaveFailed: boolean;
  mpdStatus: MpdStatus | null;
  mpdSuggestDismissed: boolean;
  offlineMode: boolean;
  persistDlna: (next: DlnaConfig) => Promise<void>;
  persistMpd: (
    next: MpdConfig,
    options?: { optimistic?: boolean },
  ) => Promise<boolean>;
  setDlnaConfig: Dispatch<SetStateAction<DlnaConfig>>;
  setLastfmKey: Dispatch<SetStateAction<string>>;
  setLastfmKeyVisible: Dispatch<SetStateAction<boolean>>;
  setLastfmLoginError: Dispatch<SetStateAction<string | null>>;
  setLastfmPassword: Dispatch<SetStateAction<string>>;
  setLastfmSaved: Dispatch<SetStateAction<boolean>>;
  setLastfmSecret: Dispatch<SetStateAction<string>>;
  setLastfmSecretVisible: Dispatch<SetStateAction<boolean>>;
  setLastfmUsername: Dispatch<SetStateAction<string>>;
  setMpdConfig: Dispatch<SetStateAction<MpdConfig>>;
  setMpdPasswordGenerated: Dispatch<SetStateAction<boolean>>;
  setMpdPasswordVisible: Dispatch<SetStateAction<boolean>>;
  setMpdSuggestDismissed: Dispatch<SetStateAction<boolean>>;
  targetGroup: string | null;
}

export function IntegrationsSection({
  activeProfile,
  discordRpc,
  dlnaConfig,
  dlnaStatus,
  dlnaUrlCopied,
  handleDlnaCopyUrl,
  handleGenerateMpdPassword,
  handleLastfmLogin,
  handleLastfmLogout,
  handleMpdCopyAddress,
  handleSaveLastfmKey,
  handleToggleDiscordRpc,
  handleToggleDlna,
  handleToggleMpd,
  handleToggleOfflineMode,
  lastfmKey,
  lastfmKeyVisible,
  lastfmLoggingIn,
  lastfmLoginError,
  lastfmPassword,
  lastfmSaved,
  lastfmSaving,
  lastfmSecret,
  lastfmSecretVisible,
  lastfmStatus,
  lastfmUsername,
  mpdAddressCopied,
  mpdConfig,
  mpdConfirmedRef,
  mpdGenerating,
  mpdHydrated,
  mpdPasswordGenerated,
  mpdPasswordInputRef,
  mpdPasswordVisible,
  mpdSaveFailed,
  mpdStatus,
  mpdSuggestDismissed,
  offlineMode,
  persistDlna,
  persistMpd,
  setDlnaConfig,
  setLastfmKey,
  setLastfmKeyVisible,
  setLastfmLoginError,
  setLastfmPassword,
  setLastfmSaved,
  setLastfmSecret,
  setLastfmSecretVisible,
  setLastfmUsername,
  setMpdConfig,
  setMpdPasswordGenerated,
  setMpdPasswordVisible,
  setMpdSuggestDismissed,
  targetGroup,
}: IntegrationsSectionProps) {
  const { t } = useTranslation();
  return (
    <section
      aria-labelledby="settings-heading-integrations"
      className="settings-category"
    >
      <SettingsCategoryHeader category="integrations" />
      <SettingsGroup id="connectivity" reveal={targetGroup === "connectivity"}>
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <WifiOff
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.offlineMode.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.offlineMode.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={offlineMode}
            onToggle={handleToggleOfflineMode}
            label={t("settings.offlineMode.title")}
          />
        </div>
      </SettingsGroup>
      <SettingsGroup id="services" reveal={targetGroup === "services"}>
        <div className="settings-block">
          <div className="flex items-start space-x-4">
            <Radio size={16} className="settings-row-icon" aria-hidden="true" />
            <div className="flex-1 min-w-0">
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.integrations.lastfm.title")}
              </div>
              <div className="text-xs mb-3 settings-description">
                {t("settings.integrations.lastfm.subtitle")}
              </div>

              {/* API key */}
              <div className="flex items-center space-x-2 mb-2">
                <div className="relative flex-1">
                  <input
                    type={lastfmKeyVisible ? "text" : "password"}
                    value={lastfmKey}
                    onChange={(e) => {
                      setLastfmKey(e.target.value);
                      setLastfmSaved(false);
                    }}
                    placeholder={t("settings.integrations.lastfm.placeholder")}
                    spellCheck={false}
                    autoComplete="off"
                    className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                  />
                  <button
                    type="button"
                    onClick={() => setLastfmKeyVisible((v) => !v)}
                    aria-label={
                      lastfmKeyVisible
                        ? t("settings.integrations.lastfm.hide")
                        : t("settings.integrations.lastfm.show")
                    }
                    className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                  >
                    {lastfmKeyVisible ? (
                      <EyeOff size={16} />
                    ) : (
                      <Eye size={16} />
                    )}
                  </button>
                </div>
              </div>

              {/* API secret */}
              <div className="flex items-center space-x-2 mb-2">
                <div className="relative flex-1">
                  <input
                    type={lastfmSecretVisible ? "text" : "password"}
                    value={lastfmSecret}
                    onChange={(e) => {
                      setLastfmSecret(e.target.value);
                      setLastfmSaved(false);
                    }}
                    placeholder={t(
                      "settings.integrations.lastfm.secretPlaceholder",
                    )}
                    spellCheck={false}
                    autoComplete="off"
                    className="w-full pr-10 pl-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                  />
                  <button
                    type="button"
                    onClick={() => setLastfmSecretVisible((v) => !v)}
                    aria-label={
                      lastfmSecretVisible
                        ? t("settings.integrations.lastfm.hide")
                        : t("settings.integrations.lastfm.show")
                    }
                    className="absolute inset-y-0 right-0 px-3 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                  >
                    {lastfmSecretVisible ? (
                      <EyeOff size={16} />
                    ) : (
                      <Eye size={16} />
                    )}
                  </button>
                </div>
                <button
                  type="button"
                  onClick={handleSaveLastfmKey}
                  disabled={lastfmSaving}
                  className={`px-4 py-2 rounded-xl text-sm font-medium transition-colors disabled:opacity-50 ${
                    lastfmSaved
                      ? "bg-emerald-500 text-white"
                      : "border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
                  }`}
                >
                  {lastfmSaved
                    ? t("settings.integrations.lastfm.saved")
                    : t("settings.integrations.lastfm.save")}
                </button>
              </div>

              {/* Account login / status — only shown once API
                    credentials are present. The session stays per
                    profile so two profiles can scrobble to two
                    different Last.fm accounts. */}
              {lastfmStatus?.configured && (
                <div className="mt-3 pt-3 border-t border-zinc-100 dark:border-zinc-800">
                  {lastfmStatus.connected ? (
                    <div className="flex items-center justify-between">
                      <div className="text-xs">
                        <span className="text-zinc-500">
                          {t("settings.integrations.lastfm.connectedAs")}{" "}
                        </span>
                        <span className="font-medium text-emerald-500">
                          {lastfmStatus.username}
                        </span>
                      </div>
                      <button
                        type="button"
                        onClick={handleLastfmLogout}
                        className="px-3 py-1.5 rounded-lg text-xs font-medium border border-zinc-200 bg-white text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors"
                      >
                        {t("settings.integrations.lastfm.disconnect")}
                      </button>
                    </div>
                  ) : (
                    <div className="space-y-2">
                      <div className="text-xs text-zinc-500">
                        {t("settings.integrations.lastfm.loginPrompt")}
                      </div>
                      <div className="flex items-center space-x-2">
                        <input
                          type="text"
                          value={lastfmUsername}
                          onChange={(e) => {
                            setLastfmUsername(e.target.value);
                            setLastfmLoginError(null);
                          }}
                          placeholder={t(
                            "settings.integrations.lastfm.usernamePlaceholder",
                          )}
                          autoComplete="username"
                          spellCheck={false}
                          className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                        />
                        <input
                          type="password"
                          value={lastfmPassword}
                          onChange={(e) => {
                            setLastfmPassword(e.target.value);
                            setLastfmLoginError(null);
                          }}
                          placeholder={t(
                            "settings.integrations.lastfm.passwordPlaceholder",
                          )}
                          autoComplete="current-password"
                          className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                        />
                        <button
                          type="button"
                          onClick={handleLastfmLogin}
                          disabled={
                            lastfmLoggingIn ||
                            !lastfmUsername.trim() ||
                            !lastfmPassword
                          }
                          className="px-4 py-2 rounded-xl text-sm font-medium bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          {lastfmLoggingIn
                            ? t("settings.integrations.lastfm.connecting")
                            : t("settings.integrations.lastfm.connect")}
                        </button>
                      </div>
                      {lastfmLoginError && (
                        <div className="text-xs text-rose-500">
                          {lastfmLoginError}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
        {/* Remounted per profile: the token is per profile, and
                    the last one's status, half-typed token or error must
                    not show under the next. */}
        <ListenBrainzCard key={activeProfile?.id ?? "none"} />
        <div className="settings-row flex items-center justify-between">
          <div className="flex items-center space-x-4">
            <Gamepad2
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div>
              <div className="text-sm font-medium text-zinc-900 dark:text-white">
                {t("settings.integrations.discord.title")}
              </div>
              <div className="text-xs settings-description">
                {t("settings.integrations.discord.subtitle")}
              </div>
            </div>
          </div>
          <ToggleSwitch
            enabled={discordRpc}
            onToggle={handleToggleDiscordRpc}
            label={t("settings.integrations.discord.title")}
          />
        </div>
      </SettingsGroup>
      <SettingsGroup id="remote" reveal={targetGroup === "remote"}>
        <RemoteServerCard />
        <RemoteTranscodeCard />
        <CatalogueMirrorCard />
        <UploadToServerCard />
        <ReconciliationCard />
      </SettingsGroup>
      <SettingsGroup id="network" advanced reveal={targetGroup === "network"}>
        <div className="settings-block">
          <div className="flex items-start space-x-4">
            <Server
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="flex-1 min-w-0">
              <div className="flex items-center justify-between mb-1">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.integrations.dlna.title")}
                  </div>
                  <div className="text-xs settings-description">
                    {t("settings.integrations.dlna.subtitle")}
                  </div>
                </div>
                <ToggleSwitch
                  enabled={dlnaConfig.enabled}
                  onToggle={handleToggleDlna}
                  label={t("settings.integrations.dlna.title")}
                />
              </div>

              {dlnaConfig.enabled && (
                <div className="mt-3 space-y-3">
                  {/* Server name */}
                  <div className="flex items-center space-x-2">
                    <label
                      htmlFor="dlna-name"
                      className="text-xs text-zinc-500 w-24 shrink-0"
                    >
                      {t("settings.integrations.dlna.serverName")}
                    </label>
                    <input
                      id="dlna-name"
                      type="text"
                      value={dlnaConfig.server_name}
                      onChange={(e) =>
                        setDlnaConfig((c) => ({
                          ...c,
                          server_name: e.target.value,
                        }))
                      }
                      onBlur={() => persistDlna(dlnaConfig)}
                      spellCheck={false}
                      className="flex-1 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 placeholder-zinc-400 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100 dark:placeholder-zinc-500"
                    />
                  </div>

                  {/* Port */}
                  <div className="flex items-center space-x-2">
                    <label
                      htmlFor="dlna-port"
                      className="text-xs text-zinc-500 w-24 shrink-0"
                    >
                      {t("settings.integrations.dlna.port")}
                    </label>
                    <input
                      id="dlna-port"
                      type="number"
                      min={0}
                      max={65535}
                      step={1}
                      value={dlnaConfig.port}
                      onChange={(e) =>
                        setDlnaConfig((c) => ({
                          ...c,
                          // A whole number in 0–65535 (0 lets the OS pick):
                          // the backend reads the port as a u16 and refuses
                          // the whole save over "12.5".
                          port: Math.max(
                            0,
                            Math.min(
                              65535,
                              Math.trunc(Number(e.target.value)) || 0,
                            ),
                          ),
                        }))
                      }
                      onBlur={() => persistDlna(dlnaConfig)}
                      className="w-32 px-3 py-2 rounded-xl text-sm bg-white border border-zinc-200 text-zinc-800 focus:outline-none focus:border-emerald-500 dark:bg-zinc-800 dark:border-zinc-700 dark:text-zinc-100"
                    />
                    <span className="text-xs settings-description">
                      {t("settings.integrations.dlna.portHint")}
                    </span>
                  </div>

                  {/* Status */}
                  <div className="flex items-center space-x-2 pt-1">
                    <span
                      className={`inline-block w-2 h-2 rounded-full ${
                        dlnaStatus?.running ? "bg-emerald-500" : "bg-zinc-400"
                      }`}
                      aria-hidden="true"
                    />
                    <span className="text-xs text-zinc-500">
                      {dlnaStatus?.running
                        ? t("settings.integrations.dlna.statusRunning")
                        : t("settings.integrations.dlna.statusStopped")}
                    </span>
                    {dlnaStatus?.bound_url && (
                      <>
                        <span className="text-xs font-mono text-zinc-700 dark:text-zinc-300 truncate">
                          {dlnaStatus.bound_url}
                        </span>
                        <button
                          type="button"
                          onClick={handleDlnaCopyUrl}
                          aria-label={t("settings.integrations.dlna.copyUrl")}
                          className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                        >
                          {dlnaUrlCopied ? (
                            <CheckIcon size={14} />
                          ) : (
                            <Copy size={14} />
                          )}
                        </button>
                      </>
                    )}
                  </div>

                  {dlnaStatus?.last_error && (
                    <div className="text-xs text-rose-500 break-words">
                      {dlnaStatus.last_error}
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
        <div className="settings-block">
          <div className="flex items-start space-x-4">
            <Network
              size={16}
              className="settings-row-icon"
              aria-hidden="true"
            />
            <div className="flex-1 min-w-0">
              <div className="flex items-center justify-between mb-1">
                <div>
                  <div className="text-sm font-medium text-zinc-900 dark:text-white">
                    {t("settings.integrations.mpd.title")}
                  </div>
                  <div className="text-xs settings-description">
                    {t("settings.integrations.mpd.subtitle")}
                  </div>
                </div>
                <ToggleSwitch
                  enabled={mpdConfig.enabled}
                  onToggle={handleToggleMpd}
                  disabled={!mpdHydrated || mpdGenerating}
                  label={t("settings.integrations.mpd.title")}
                />
              </div>

              {/* Outside the enabled block: a switch-on that fails
                      rolls `enabled` back, and the reason must not vanish
                      with the block. */}
              {mpdSaveFailed && (
                <div
                  role="alert"
                  className="mt-2 text-xs text-rose-500 break-words"
                >
                  {t("settings.integrations.mpd.saveFailed")}
                </div>
              )}

              {mpdConfig.enabled && (
                <div className="mt-3 space-y-3">
                  {/* The LAN exposure is stated plainly rather than
                          buried in docs — it is the whole point of the
                          feature and also its only real risk. With a password
                          set (MpdConfig.requires_auth), control is gated, so
                          the "anyone can control" warning would be misleading:
                          swap it for one that separates reachability from
                          control. */}
                  <div className="text-xs text-amber-600 dark:text-amber-500">
                    {mpdConfig.password
                      ? t("settings.integrations.mpd.lanWarningAuth")
                      : t("settings.integrations.mpd.lanWarning")}
                  </div>

                  {/* No password on a server that answers the whole
                          network: offer one right where it is turned on,
                          generated or typed. Gated on the hydrated config so
                          it never flashes over a password still loading. */}
                  {mpdHydrated &&
                    !mpdConfig.password &&
                    !mpdSuggestDismissed && (
                      <div
                        role="group"
                        aria-labelledby="mpd-suggest-title"
                        className="rounded-xl border border-amber-500/30 bg-amber-500/5 p-3 space-y-2"
                      >
                        <div
                          id="mpd-suggest-title"
                          className="text-xs font-medium text-zinc-900 dark:text-white"
                        >
                          {t("settings.integrations.mpd.suggestTitle")}
                        </div>
                        <div className="text-xs settings-description">
                          {t("settings.integrations.mpd.suggestBody")}
                        </div>
                        <div className="flex flex-wrap gap-2">
                          <button
                            type="button"
                            disabled={mpdGenerating}
                            onClick={() => void handleGenerateMpdPassword()}
                            className="px-3 py-1 text-xs rounded-lg bg-emerald-500 text-white hover:bg-emerald-600 disabled:opacity-50 disabled:cursor-not-allowed"
                          >
                            {t("settings.integrations.mpd.suggestGenerate")}
                          </button>
                          <button
                            type="button"
                            onClick={() => mpdPasswordInputRef.current?.focus()}
                            className="px-3 py-1 text-xs rounded-lg bg-zinc-100 dark:bg-zinc-800 text-zinc-700 dark:text-zinc-200 hover:bg-zinc-200 dark:hover:bg-zinc-700"
                          >
                            {t("settings.integrations.mpd.suggestChoose")}
                          </button>
                          <button
                            type="button"
                            onClick={() => {
                              setMpdSuggestDismissed(true);
                              mpdPasswordInputRef.current?.focus();
                            }}
                            className="px-3 py-1 text-xs rounded-lg text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200"
                          >
                            {t("settings.integrations.mpd.suggestDismiss")}
                          </button>
                        </div>
                      </div>
                    )}

                  <div className="flex items-center space-x-2">
                    <label
                      htmlFor="mpd-port"
                      className="text-xs text-zinc-500 w-24 shrink-0"
                    >
                      {t("settings.integrations.mpd.port")}
                    </label>
                    <input
                      id="mpd-port"
                      type="number"
                      min={1}
                      max={65535}
                      step={1}
                      disabled={!mpdHydrated || mpdGenerating}
                      value={mpdConfig.port}
                      onChange={(e) =>
                        setMpdConfig({
                          ...mpdConfig,
                          // Keep the state an integer in 0–65535 while
                          // typing; onBlur enforces the final 1–65535.
                          port: Math.max(
                            0,
                            Math.min(
                              65535,
                              Math.trunc(Number(e.target.value)) || 0,
                            ),
                          ),
                        })
                      }
                      onBlur={() => {
                        // Never persist an empty / zero / decimal /
                        // out-of-range port: fall back to the last
                        // confirmed value (or the 6600 default).
                        const p = mpdConfig.port;
                        const valid =
                          Number.isInteger(p) && p >= 1 && p <= 65535
                            ? p
                            : (mpdConfirmedRef.current?.port ?? 6600);
                        const next = { ...mpdConfig, port: valid };
                        setMpdConfig(next);
                        void persistMpd(next);
                      }}
                      className="w-28 px-2 py-1 text-xs rounded-lg bg-zinc-100 dark:bg-zinc-800 border border-transparent focus:border-emerald-500 focus:outline-none text-zinc-900 dark:text-white"
                    />
                    <span className="text-xs settings-description">
                      {t("settings.integrations.mpd.portHint")}
                    </span>
                  </div>

                  <div className="flex items-center space-x-2">
                    <label
                      htmlFor="mpd-password"
                      className="text-xs text-zinc-500 w-24 shrink-0"
                    >
                      {t("settings.integrations.mpd.password")}
                    </label>
                    <div className="relative flex-1 min-w-0">
                      <input
                        id="mpd-password"
                        ref={mpdPasswordInputRef}
                        type={mpdPasswordVisible ? "text" : "password"}
                        autoComplete="new-password"
                        spellCheck={false}
                        disabled={!mpdHydrated || mpdGenerating}
                        value={mpdConfig.password}
                        placeholder={t(
                          "settings.integrations.mpd.passwordPlaceholder",
                        )}
                        onChange={(e) => {
                          setMpdPasswordGenerated(false);
                          setMpdConfig({
                            ...mpdConfig,
                            password: e.target.value,
                          });
                        }}
                        onBlur={() => persistMpd(mpdConfig)}
                        className="w-full pl-2 pr-8 py-1 text-xs rounded-lg bg-zinc-100 dark:bg-zinc-800 border border-transparent focus:border-emerald-500 focus:outline-none text-zinc-900 dark:text-white"
                      />
                      <button
                        type="button"
                        onClick={() => setMpdPasswordVisible((v) => !v)}
                        aria-label={
                          mpdPasswordVisible
                            ? t("settings.integrations.mpd.hidePassword")
                            : t("settings.integrations.mpd.showPassword")
                        }
                        className="absolute inset-y-0 right-0 px-2 flex items-center text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                      >
                        {mpdPasswordVisible ? (
                          <EyeOff size={14} />
                        ) : (
                          <Eye size={14} />
                        )}
                      </button>
                    </div>
                  </div>
                  <div
                    aria-live="polite"
                    className="text-xs settings-description"
                  >
                    {mpdPasswordGenerated && mpdConfig.password
                      ? t("settings.integrations.mpd.generatedHint")
                      : t("settings.integrations.mpd.passwordHint")}
                  </div>

                  {/* Status */}
                  <div className="flex items-center space-x-2 pt-1">
                    <span
                      className={`inline-block w-2 h-2 rounded-full ${
                        mpdStatus?.running ? "bg-emerald-500" : "bg-zinc-400"
                      }`}
                      aria-hidden="true"
                    />
                    <span className="text-xs text-zinc-500">
                      {mpdStatus?.running
                        ? t("settings.integrations.mpd.statusRunning")
                        : t("settings.integrations.mpd.statusStopped")}
                    </span>
                    {mpdStatus?.bound_address && (
                      <>
                        <span className="text-xs font-mono text-zinc-700 dark:text-zinc-300 truncate">
                          {mpdStatus.bound_address}
                        </span>
                        <button
                          type="button"
                          onClick={handleMpdCopyAddress}
                          aria-label={t(
                            "settings.integrations.mpd.copyAddress",
                          )}
                          className="p-1 rounded text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
                        >
                          {mpdAddressCopied ? (
                            <CheckIcon size={14} />
                          ) : (
                            <Copy size={14} />
                          )}
                        </button>
                      </>
                    )}
                  </div>

                  {mpdStatus?.last_error && (
                    <div className="text-xs text-rose-500 break-words">
                      {mpdStatus.last_error}
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
      </SettingsGroup>
    </section>
  );
}
