use serde::{Serialize, Serializer};

/// Top-level error type for the Tauri backend.
///
/// Implements [`serde::Serialize`] so it can be returned from Tauri commands.
/// The wire format is a single `String` (the `Display` representation),
/// except for [`AppError::Shown`], the error the interface says in the
/// user's language (see the `Serialize` impl).
///
/// During the Phase 1.a refactor (RFC-001) this type lives at the
/// boundary: it wraps `waveflow_core::error::CoreError` for everything
/// portable (storage, IO, profile invariants) and carries the
/// Tauri-specific variants that have no place in a `waveflow-server`
/// build (`tauri::Error`, `MissingAppDataDir` from `dirs::data_dir()`,
/// the audio engine's `cpal`/`rubato` error wrappers). The legacy
/// generic variants (`Database`, `Io`, `ProfileNotFound`, …) are kept
/// here for now so existing call sites continue to compile; future
/// commits migrate them to `CoreError` as their owning modules move
/// into `crates/core`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Errors raised by `waveflow-core` functions. Stays at the top of
    /// the enum so reviewers immediately see where new error sources
    /// land once the bulk of the migration completes.
    #[error(transparent)]
    Core(#[from] waveflow_core::error::CoreError),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// The database holds a migration this build doesn't ship — it was
    /// written by a newer version of WaveFlow. Split out of the generic
    /// `Migration` variant because it's the one migration failure with
    /// a remedy the user can act on, and startup turns it into a dialog
    /// instead of a panic (issue #526). Raised by
    /// [`crate::db::schema_guard::ensure_not_from_the_future`].
    #[error(
        "{} was written by a newer version of WaveFlow (database schema {version})",
        scope.label()
    )]
    SchemaFromTheFuture {
        /// Which database — decides what the user can actually do about
        /// it, so startup branches the remedy on it.
        scope: crate::db::schema_guard::DbScope,
        /// The highest applied migration this build doesn't know.
        version: i64,
        /// When that migration was applied, `YYYY-MM-DD HH:MM:SS` as
        /// SQLite recorded it. `None` if the column was NULL.
        installed_on: Option<String>,
    },

    /// A migration this build *does* ship was applied in a different
    /// form — same version, different SQL. The other way a downgrade
    /// (or a beta whose migration was amended before release) lands,
    /// and the one sqlx reports as `VersionMismatch`. Split out for the
    /// same reason as [`AppError::SchemaFromTheFuture`]: it has a
    /// remedy, and the alternative was a panic out of the Tauri `setup`
    /// hook. Raised by
    /// [`crate::db::schema_guard::ensure_no_foreign_migration`].
    #[error(
        "{} was written by a different build of WaveFlow (migration {version} does not match)",
        scope.label()
    )]
    SchemaWrittenElsewhere {
        /// Which database — decides what the user can do about it.
        scope: crate::db::schema_guard::DbScope,
        /// The first applied migration whose SQL differs from ours.
        version: i64,
    },

    /// A database with migrations pending had no copy to go back to,
    /// and the user chose — or nobody was there to be asked — not to
    /// migrate it without one. Nothing was changed. Raised by
    /// [`crate::db::pre_migration_backup::ensure_backed_up`].
    #[error(
        "{} was not updated: no backup of it could be made first, so it was left as it was",
        scope.label()
    )]
    PreMigrationBackupDeclined {
        /// Which database stayed un-migrated.
        scope: crate::db::schema_guard::DbScope,
    },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tauri error: {0}")]
    Tauri(#[from] tauri::Error),

    #[error("profile not found: id={0}")]
    ProfileNotFound(i64),

    // Used by upcoming library/scan/queue commands that require an active
    // profile. Referenced here so the variant stays in the public API.
    #[allow(dead_code)]
    #[error("no profile is currently active")]
    NoActiveProfile,

    /// A command was told which profile the caller believed was active
    /// and it isn't anymore. Raised by
    /// [`AppState::require_profile_pool_for`](crate::state::AppState::require_profile_pool_for)
    /// so a write queued before a `switch_profile` lands nowhere instead
    /// of in the new profile (issue #485). Callers treat it as a benign
    /// no-op — the UI re-reads the new profile's value anyway.
    #[error("active profile changed (expected id={expected}, active id={active:?})")]
    ProfileChanged { expected: i64, active: Option<i64> },

    #[error("app data directory is unavailable")]
    MissingAppDataDir,

    #[error("audio error: {0}")]
    Audio(String),

    /// An exclusive output the device would not give, and why, where the
    /// backend could tell. The output layer reads the reason when it
    /// falls back to shared mode, so the user is told something more
    /// useful than "refused".
    #[error("audio error: {detail}")]
    ExclusiveRefused {
        reason: crate::audio::engine::ExclusiveRefusal,
        detail: String,
    },

    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// The tag editor's save found the file no longer the one its dialog
    /// read: another program wrote it in the meantime. Refused rather
    /// than overwriting that change with the values the dialog showed.
    /// The frontend recognises this message (`FILE_CHANGED_ON_DISK` in
    /// `src/lib/tauri/track.ts`) and offers to reload, so the wording is
    /// part of the contract.
    #[error("the file changed on disk since the editor opened it")]
    FileChangedOnDisk,

    /// A folder that overlaps a library folder: the same one, one under
    /// it, or one holding it ([`waveflow_core::scanner::folders`]). Both
    /// would claim the files they share. The frontend recognises the
    /// message (`parseFolderOverlap` in `src/lib/tauri/library.ts`) to
    /// say which folder is in the way, so the wording is part of the
    /// contract.
    #[error(
        "the folder {} the library folder {existing}",
        overlap_phrase(relation)
    )]
    FolderOverlap {
        relation: waveflow_core::scanner::folders::FolderRelation,
        existing: String,
    },

    /// A library folder whose scan stopped before it wrote anything,
    /// because nothing it could list says what became of its tracks. They
    /// stay available.
    #[error("the library folder {path} cannot be read: {reason}")]
    FolderUnreachable { path: String, reason: String },

    /// A remote server answered a token refresh with this status. Its own variant
    /// because a 401 means the account is signed out, where any other failure of a
    /// refresh says nothing about it.
    #[cfg(feature = "sync_v2")]
    #[error("token refresh rejected with status {0}")]
    RemoteRefreshRejected(u16),

    /// A token refresh that got no answer anyone can read, so nothing is known of the
    /// account.
    #[cfg(feature = "sync_v2")]
    #[error("token refresh failed: {0}")]
    RemoteRefreshUnanswered(String),

    /// An error the interface shows the user, and says in their language.
    /// The `kind` names the sentence (`errors.<kind>` in the locales), the
    /// `params` fill its placeholders, and the English `message` is what
    /// the log and a bug report read, and what the interface falls back to
    /// for a kind it does not know. Built with [`AppError::shown`]; the
    /// only variant that does not cross the IPC as a bare string (see the
    /// `Serialize` impl below).
    #[error("{message}")]
    Shown {
        kind: ShownKind,
        message: String,
        params: Vec<(&'static str, String)>,
    },

    #[error("{0}")]
    Other(String),
}

/// Declares [`ShownKind`] and its `ShownKind::ALL` from one list, so
/// the test that looks for each kind's sentence cannot miss one.
macro_rules! error_kinds {
    ($($(#[$doc:meta])* $kind:ident,)*) => {
        /// What an [`AppError::Shown`] is about: the sentence the interface
        /// says instead of the English message. Serialized in `snake_case`,
        /// which is the key under `errors` in every locale; a kind added
        /// here is added to the 17 locales, which a test checks.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum ShownKind {
            $($(#[$doc])* $kind,)*
        }

        impl ShownKind {
            /// Every kind, in declaration order.
            #[cfg(test)]
            pub const ALL: &[ShownKind] = &[$(Self::$kind,)*];
        }
    };
}

error_kinds! {
    /// Offline mode refused a request to the network, with nothing more
    /// specific to say about what it was for.
    Offline,
    /// Offline mode refused the plugin store's catalogue.
    OfflinePluginStore,
    /// Offline mode refused a plugin's download.
    OfflinePluginInstall,
    /// Offline mode refused applying a MusicBrainz match.
    OfflineEnrichment,
    /// Offline mode refused fetching tags from MusicBrainz.
    OfflineTagFetch,
    /// Offline mode refused a MusicBrainz match in the tag workshop.
    OfflineMusicbrainz,
    /// Offline mode refused signing in to Last.fm.
    OfflineLastfm,
    /// Offline mode refused connecting ListenBrainz.
    OfflineListenbrainz,
    /// Offline mode refused downloading the Web Radio station catalogue.
    OfflineRadioCatalogue,
    /// Offline mode refused signing in to a server. Raised by the remote
    /// source, which only a `sync_v2` build has.
    #[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
    OfflineServerSignIn,
    /// Offline mode refused reaching a server. `sync_v2` only, like the
    /// one above.
    #[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
    OfflineServer,
    /// The ListenBrainz token field was left empty.
    ListenbrainzTokenEmpty,
    /// ListenBrainz did not accept the token.
    ListenbrainzTokenRejected,
    /// An image whose format is not JPEG, PNG or WebP.
    UnsupportedImage,
    /// A picture file with nothing in it.
    ImageFileEmpty,
    /// A video the user picked that is not an MP4.
    UnsupportedVideo,
    /// A file the user picked is over the size the app takes; `max` is
    /// that size, written for a person ("10.0 MB").
    FileTooLarge,
    /// A track on a network share whose bytes are not cached on this
    /// machine yet.
    NetworkTrackNotCached,
    /// A cover change asked of a generated smart playlist (Daily Mix, On
    /// Repeat, Release Radar, a themed mix), whose generator owns it.
    GeneratedPlaylistCover,
}

impl AppError {
    /// An error the interface says in the user's language: `kind` names
    /// the sentence, `message` is the English one for the log.
    pub fn shown(kind: ShownKind, message: impl Into<String>) -> Self {
        Self::Shown {
            kind,
            message: message.into(),
            params: Vec::new(),
        }
    }

    /// Adds a value for the sentence's `{{name}}` placeholder to an error
    /// built by [`AppError::shown`]. Any other error is returned as it was.
    pub fn with_param(mut self, name: &'static str, value: impl Into<String>) -> Self {
        if let Self::Shown { params, .. } = &mut self {
            params.push((name, value.into()));
        }
        self
    }
}

fn overlap_phrase(relation: &waveflow_core::scanner::folders::FolderRelation) -> &'static str {
    use waveflow_core::scanner::folders::FolderRelation;
    match relation {
        FolderRelation::Same => "is",
        FolderRelation::Inside => "is inside",
        FolderRelation::Contains => "contains",
    }
}

/// The wire format is the `Display` string for every variant but
/// [`AppError::Shown`], which crosses as `{ kind, message, params }`
/// (`params` an object of strings). The frontend's `invoke` wrapper
/// (`src/lib/tauri/invoke.ts`) turns that object into an `Error` whose
/// text is `message`, so a caller that reads an error as text reads what
/// it always did.
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;

        match self {
            Self::Shown {
                kind,
                message,
                params,
            } => {
                let mut shown = serializer.serialize_struct("AppError", 3)?;
                shown.serialize_field("kind", kind)?;
                shown.serialize_field("message", message)?;
                shown.serialize_field("params", &Params(params))?;
                shown.end()
            }
            other => serializer.serialize_str(&other.to_string()),
        }
    }
}

/// An error's placeholder values, as a JSON object.
struct Params<'a>(&'a [(&'static str, String)]);

impl Serialize for Params<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_map(self.0.iter().map(|(name, value)| (*name, value)))
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_shown_error_crosses_as_its_kind_message_and_params() {
        let err = AppError::shown(
            ShownKind::OfflinePluginStore,
            "offline mode is on; the plugin store is unavailable",
        );
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({
                "kind": "offline_plugin_store",
                "message": "offline mode is on; the plugin store is unavailable",
                "params": {},
            })
        );
    }

    #[test]
    fn a_shown_error_carries_its_params_by_name() {
        let err = AppError::shown(
            ShownKind::FileTooLarge,
            "file too large: 12.0 MB (max 10.0 MB)",
        )
        .with_param("max", "10.0 MB")
        .with_param("actual", "12.0 MB");
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!({
                "kind": "file_too_large",
                "message": "file too large: 12.0 MB (max 10.0 MB)",
                "params": { "max": "10.0 MB", "actual": "12.0 MB" },
            })
        );
        // The log reads the English message, never the params.
        assert_eq!(err.to_string(), "file too large: 12.0 MB (max 10.0 MB)");
    }

    #[test]
    fn a_shown_error_logs_its_english_message() {
        let err = AppError::shown(ShownKind::Offline, "offline mode is enabled");
        assert_eq!(err.to_string(), "offline mode is enabled");
    }

    #[test]
    fn every_other_error_still_crosses_as_its_text() {
        let err = AppError::Other("something broke".into());
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!("something broke")
        );
        // `with_param` leaves an error that has no sentence alone.
        let err = AppError::Other("something broke".into()).with_param("path", "/music");
        assert_eq!(
            serde_json::to_value(&err).unwrap(),
            json!("something broke")
        );
    }

    #[test]
    fn kinds_are_named_in_snake_case() {
        // The locales' keys spell these; a rename here is a rename there.
        for (kind, word) in [
            (ShownKind::Offline, "offline"),
            (ShownKind::OfflineMusicbrainz, "offline_musicbrainz"),
            (ShownKind::FileTooLarge, "file_too_large"),
            (
                ShownKind::ListenbrainzTokenEmpty,
                "listenbrainz_token_empty",
            ),
            (ShownKind::NetworkTrackNotCached, "network_track_not_cached"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(word));
        }
    }

    #[test]
    fn every_kind_has_a_sentence_in_every_locale() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../src/i18n/locales");
        let mut locales = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            locales += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            let locale: serde_json::Value = serde_json::from_str(&text).unwrap();
            for kind in ShownKind::ALL {
                let key = serde_json::to_value(kind).unwrap();
                let key = key.as_str().unwrap();
                let sentence = locale["errors"][key].as_str();
                assert!(
                    sentence.is_some_and(|s| !s.trim().is_empty()),
                    "{} has no errors.{key}",
                    path.display()
                );
            }
        }
        assert_eq!(locales, 17, "a locale was added or lost");
    }
}
