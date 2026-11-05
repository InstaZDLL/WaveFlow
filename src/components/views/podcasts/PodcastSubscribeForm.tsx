import { useId, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Plus } from "lucide-react";

import { podcastFailureMessage } from "../../../lib/podcastWords";
import { podcastErrorWord, podcastSubscribe } from "../../../lib/tauri/podcast";

interface PodcastSubscribeFormProps {
  onSubscribed: (channelId: number) => void;
  disabled?: boolean;
  /** The line under the field, when the form's place has more to say. */
  hint?: string;
}

/**
 * The address of a feed, typed by a person. Fetched before anything is stored,
 * so a typo leaves nothing behind. Subscribing again to the address of a channel
 * a restored profile holds gives it its addresses back and keeps every position.
 */
export function PodcastSubscribeForm({
  onSubscribed,
  disabled = false,
  hint,
}: PodcastSubscribeFormProps) {
  const { t } = useTranslation();
  const fieldId = useId();
  const errorId = useId();
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const typed = address.trim();
    if (!typed || busy || disabled) return;
    setBusy(true);
    setError(null);
    try {
      const channel = await podcastSubscribe(typed);
      setAddress("");
      onSubscribed(channel.id);
    } catch (err) {
      const word = podcastErrorWord(err);
      // The rejection is a word or an error of ours; the address is never logged.
      if (word == null) console.error("[podcasts] subscribing failed");
      setError(podcastFailureMessage(t, word));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className="pc-subscribe"
      noValidate
      onSubmit={(event) => void submit(event)}
    >
      <label className="pc-label" htmlFor={fieldId}>
        {t("podcasts.subscribe.label")}
      </label>
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <input
          id={fieldId}
          type="url"
          inputMode="url"
          spellCheck={false}
          autoComplete="off"
          value={address}
          disabled={disabled || busy}
          onChange={(event) => {
            setAddress(event.target.value);
            setError(null);
          }}
          placeholder="https://"
          aria-invalid={error != null}
          aria-describedby={error ? errorId : undefined}
          className="pc-field min-w-[16rem] flex-1"
        />
        <button
          type="submit"
          className="pc-button"
          data-primary
          disabled={disabled || busy || address.trim() === ""}
        >
          {busy ? (
            <Loader2 size={15} className="animate-spin" aria-hidden="true" />
          ) : (
            <Plus size={15} aria-hidden="true" />
          )}
          {t("podcasts.subscribe.action")}
        </button>
      </div>
      {error && (
        <p id={errorId} role="alert" className="pc-error mt-2">
          {error}
        </p>
      )}
      <p className="pc-meta mt-2">{hint ?? t("podcasts.subscribe.hint")}</p>
    </form>
  );
}
