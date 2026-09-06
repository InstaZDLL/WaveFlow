import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";
import { Loader2, Save, Server, X } from "lucide-react";
import { useModalA11y } from "../../hooks/useModalA11y";
import {
  remoteGetTrackTags,
  remoteUpdateTrackTags,
} from "../../lib/tauri/remoteServer";

interface RemoteTrackTagsModalProps {
  /** Server identifier of the track to correct. `null` keeps it closed. */
  trackId: string | null;
  onClose: () => void;
  /** Fired after a successful save, so the caller can refresh its list. */
  onSaved?: () => void;
}

type FormState = {
  title: string;
  artist: string;
  genre: string;
  year: string;
  track_number: string;
  disc_number: string;
};

const EMPTY: FormState = {
  title: "",
  artist: "",
  genre: "",
  year: "",
  track_number: "",
  disc_number: "",
};

/**
 * A typed number field, read as one of three things — and the third is
 * why this does not simply return `number | null`.
 *
 * Blank means "no correction". A whole number means that number. Anything
 * else — "1.5", "1e400", a stray letter, all of which a `type="number"`
 * input will hold — is **refused**, because the alternative is worse than
 * it looks: folding it into `null` would send "no correction" under a
 * wholesale patch, silently *withdrawing* the correction the user believes
 * they have just typed.
 *
 * `parseInt` is the wrong tool here for the same family of reason: it
 * stops at the first character it cannot use, so "1.9" reads as 1 and
 * "1e3" as 1. `Number` reads the whole string or nothing, and
 * `isSafeInteger` rejects fractions and anything past the range an integer
 * round-trips.
 */
type ParsedNumber = { ok: true; value: number | null } | { ok: false };

function parseWhole(value: string): ParsedNumber {
  const trimmed = value.trim();
  if (trimmed === "") return { ok: true, value: null };
  const parsed = Number(trimmed);
  return Number.isSafeInteger(parsed) ? { ok: true, value: parsed } : { ok: false };
}

/**
 * Correct a server track's metadata.
 *
 * Deliberately not the local Properties dialog. That one is a file
 * inspector — codec, bit depth, path, size, on-disk analysis — and for
 * a track on somebody else's disk almost none of it exists. This shows
 * the six fields the server can actually store and nothing else.
 *
 * ## The whole form is sent every time
 *
 * The server's patch is wholesale: the body states the corrections the
 * track should carry afterwards, so a field left out is a correction
 * *withdrawn*, not a field left alone. Sending only what changed —
 * which is what the local editor does — would silently drop the rest.
 *
 * ## Clearing a field shows a blank that may not last
 *
 * Emptying a box withdraws the correction, and the track falls back to
 * whatever its file's tag says. This device has never seen that value,
 * only the correction that was masking it, so the blank stands until
 * the server's reply lands and the real tag reappears.
 */
export function RemoteTrackTagsModal({
  trackId,
  onClose,
  onSaved,
}: RemoteTrackTagsModalProps) {
  const { t } = useTranslation();
  const isOpen = trackId != null;
  const [form, setForm] = useState<FormState>(EMPTY);
  // The track the form was filled from. Anything else — including
  // `null` — means what is on screen does not describe `trackId` yet.
  // Stamped rather than flagged: a `loading` boolean would have to be
  // raised synchronously inside the effect, which cascades renders, and
  // it could not tell "still loading" from "loaded, for another track".
  const [loadedId, setLoadedId] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<{
    id: string;
    message: string;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  // Load what the mirror holds. Guarded on the identifier and not only
  // on unmount: a reply resolving after the modal has moved to another
  // track would otherwise fill that track's form with this one's
  // values.
  useEffect(() => {
    if (trackId == null) return;
    let cancelled = false;
    remoteGetTrackTags(trackId)
      .then((tags) => {
        if (cancelled) return;
        if (tags == null) {
          // The mirror holds no row for this track. An empty form is not
          // the answer: under a wholesale patch, saving it would withdraw
          // every correction the track carries. Reported like any other
          // failure to load, which leaves saving disabled.
          setLoadError({ id: trackId, message: t("remote.tags.missing") });
          return;
        }
        setForm({
          title: tags.title,
          artist: tags.artist ?? "",
          genre: tags.genre ?? "",
          year: tags.year?.toString() ?? "",
          track_number: tags.track_number?.toString() ?? "",
          disc_number: tags.disc_number?.toString() ?? "",
        });
        setLoadedId(trackId);
      })
      .catch((err) => {
        console.error("[RemoteTrackTags] remote_get_track_tags failed", err);
        if (!cancelled) setLoadError({ id: trackId, message: String(err) });
      });
    return () => {
      cancelled = true;
    };
  }, [trackId, t]);

  // A failed load leaves the form blank, and blank is not neutral here:
  // the patch is wholesale, so saving it would withdraw every
  // correction the track carries. Saving therefore waits for a form
  // that was actually filled from this track.
  const ready = trackId != null && loadedId === trackId;
  const failed = trackId != null && loadError?.id === trackId;
  const error = failed ? loadError.message : saveError;

  const handleClose = useCallback(() => {
    if (saving) return;
    onClose();
  }, [saving, onClose]);

  // Escape goes through the same guard as the button. Handing `onClose`
  // straight to the hook would let Escape hide the dialog mid-save, and
  // a request that then failed would write its error onto a dialog
  // nobody can see.
  const dialogRef = useModalA11y<HTMLDivElement>(isOpen, handleClose);

  const handleSave = useCallback(async () => {
    if (trackId == null) return;
    const year = parseWhole(form.year);
    const trackNumber = parseWhole(form.track_number);
    const discNumber = parseWhole(form.disc_number);
    if (!year.ok || !trackNumber.ok || !discNumber.ok) {
      // Refused rather than coerced. Sending these as `null` would read
      // as "no correction" and withdraw the very field being edited.
      setSaveError(t("remote.tags.invalidNumber"));
      return;
    }
    setSaving(true);
    setSaveError(null);
    try {
      await remoteUpdateTrackTags(trackId, {
        title: form.title,
        artist: form.artist,
        genre: form.genre,
        year: year.value,
        track_number: trackNumber.value,
        disc_number: discNumber.value,
      });
      onSaved?.();
      onClose();
    } catch (err) {
      console.error("[RemoteTrackTags] remote_update_track_tags failed", err);
      setSaveError(String(err));
    } finally {
      setSaving(false);
    }
  }, [trackId, form, onSaved, onClose, t]);

  // Numbers are checked as they are typed as well as on save, so the
  // button says "not yet" instead of the dialog saying "no" afterwards.
  const numbersValid =
    parseWhole(form.year).ok &&
    parseWhole(form.track_number).ok &&
    parseWhole(form.disc_number).ok;

  if (!isOpen) return null;

  // Labels come from the local tag editor rather than a set of our own:
  // they name the same six fields, and a second set would drift against
  // the first in seventeen locales before anyone noticed.
  const fields: {
    id: keyof FormState;
    label: string;
    numeric: boolean;
    wide: boolean;
  }[] = [
    {
      id: "title",
      label: "trackProperties.fields.title",
      numeric: false,
      wide: true,
    },
    {
      id: "artist",
      label: "trackProperties.fields.artist",
      numeric: false,
      wide: true,
    },
    {
      id: "genre",
      label: "trackProperties.fields.genre",
      numeric: false,
      wide: true,
    },
    { id: "year", label: "trackProperties.year", numeric: true, wide: false },
    {
      id: "track_number",
      label: "trackProperties.fields.trackNumber",
      numeric: true,
      wide: false,
    },
    {
      id: "disc_number",
      label: "trackProperties.fields.discNumber",
      numeric: true,
      wide: false,
    },
  ];

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      transition={{ duration: 0.18, ease: "easeOut" }}
      className="fixed inset-0 z-100 bg-black/80 backdrop-blur-md flex items-center justify-center p-4"
      onClick={handleClose}
    >
      <motion.div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="remote-track-tags-title"
        initial={{ opacity: 0, scale: 0.95, y: 8 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 380, damping: 28, mass: 0.6 }}
        className="relative w-full max-w-lg rounded-3xl border border-zinc-200 bg-white p-6 shadow-2xl dark:border-zinc-800 dark:bg-surface-dark-elevated max-h-[90vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-4 mb-5">
          <div className="flex items-center gap-3">
            <div className="w-10 h-10 rounded-xl bg-emerald-500/10 text-emerald-500 flex items-center justify-center">
              <Server size={18} />
            </div>
            <div>
              <h2
                id="remote-track-tags-title"
                className="text-lg font-bold text-zinc-900 dark:text-white"
              >
                {t("remote.tags.title")}
              </h2>
              <p className="text-xs text-zinc-500">
                {t("remote.tags.subtitle")}
              </p>
            </div>
          </div>
          <button
            type="button"
            onClick={handleClose}
            aria-label={t("common.close")}
            className="p-2 rounded-full text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-700 dark:hover:text-zinc-200 transition-colors"
          >
            <X size={16} />
          </button>
        </div>

        <p className="text-xs text-zinc-500 dark:text-zinc-400 mb-4">
          {t("remote.tags.help")}
        </p>

        <div className="grid grid-cols-2 gap-3">
          {fields.map(({ id, label, numeric, wide }) => (
            <label key={id} className={wide ? "col-span-2" : "col-span-1"}>
              <span className="block text-xs font-semibold tracking-wider uppercase text-zinc-500 mb-1">
                {t(label)}
              </span>
              <input
                type={numeric ? "number" : "text"}
                value={form[id]}
                disabled={!ready || saving}
                onChange={(e) => {
                  const value = e.currentTarget.value;
                  setForm((prev) => ({ ...prev, [id]: value }));
                }}
                className="w-full px-2 py-1.5 rounded-md text-sm bg-white dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 disabled:opacity-50 focus:outline-none focus:ring-2 focus:ring-emerald-500"
              />
            </label>
          ))}
        </div>

        {error && (
          <div
            role="alert"
            className="mt-4 p-3 rounded-xl bg-red-50 dark:bg-red-900/20 text-xs text-red-600 dark:text-red-400"
          >
            {error}
          </div>
        )}

        <div className="flex items-center justify-end gap-2 mt-5">
          <button
            type="button"
            onClick={handleClose}
            disabled={saving}
            className="px-4 py-2 rounded-xl text-sm font-medium text-zinc-500 hover:text-zinc-800 dark:text-zinc-400 dark:hover:text-zinc-200 transition-colors disabled:opacity-50"
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={!ready || saving || !numbersValid}
            className="px-5 py-2 rounded-xl text-sm font-semibold text-white bg-emerald-500 hover:bg-emerald-600 shadow-lg transition-colors flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {saving ? (
              <Loader2 size={14} className="animate-spin" />
            ) : (
              <Save size={14} />
            )}
            <span>{t("common.save")}</span>
          </button>
        </div>
      </motion.div>
    </motion.div>
  );
}
