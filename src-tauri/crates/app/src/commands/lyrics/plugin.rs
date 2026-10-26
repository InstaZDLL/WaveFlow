//! Lyrics from the `waveflow:metadata` plugins, and caching the bundle
//! a plugin answers with.

use std::time::Duration;

use waveflow_core::plugin::runtime::{
    AssociatedKind as PluginAssociatedKind, LyricsBundle as PluginLyricsBundle,
    LyricsDocFormat as PluginLyricsFormat,
};

use crate::{error::AppResult, state::AppState};

use super::*;

/// Marks a `lyrics.provider` value as a plugin id rather than one of the
/// built-in network providers. The two namespaces share a column and
/// nothing else: `Provider::from_id` knows only its own.
pub(super) const PLUGIN_PROVIDER_PREFIX: &str = "plugin:";

/// Map the `waveflow:metadata/v2` format enum onto the host's.
fn plugin_format_to_app(f: PluginLyricsFormat) -> LyricsFormat {
    match f {
        PluginLyricsFormat::Plain => LyricsFormat::Plain,
        PluginLyricsFormat::Lrc => LyricsFormat::Lrc,
        PluginLyricsFormat::EnhancedLrc => LyricsFormat::EnhancedLrc,
        PluginLyricsFormat::Ttml => LyricsFormat::Ttml,
    }
}

/// The whole of the host's trust in a plugin's lyrics document.
///
/// A plugin hands over bytes and a label; nothing parses the document
/// into lines and words on this side, because the renderer already does
/// that and a second model in Rust would only be a second thing to keep
/// correct. So the check is the one that can be made cheaply and here:
/// re-sniff the content and refuse it unless the answer matches what was
/// declared. `detect_format` is the same routine the embedded-tag and
/// sidecar tiers already trust, and it is deliberately conservative —
/// brackets with no timestamp stay `plain` rather than being promoted to
/// `lrc`.
///
/// What this catches: a document truncated in transit, a provider that
/// changed format without saying, an `ttml` label on an HTML error page.
/// What it does not: TTML that parses as XML and means nothing. That
/// failure surfaces in the renderer, which is the only place that could
/// have judged it anyway.
pub(super) fn document_is_what_it_claims(content: &str, declared: LyricsFormat) -> bool {
    !content.trim().is_empty() && detect_format(content) == declared
}

/// Persist one fetch result — the primary document and everything that
/// came with it — as a single unit.
///
/// The bundle replaces whatever was cached for this file: the associated
/// rows are deleted before the new ones land, inside the same
/// transaction as the primary upsert. Without that, switching providers
/// would leave the previous one's translation sitting under the new
/// original, and the panel would show two documents that were never
/// published together.
///
/// Documents that fail [`document_is_what_it_claims`] are dropped, not
/// fatal: a provider getting one translation wrong should cost that
/// translation, not the lyrics. Each rejection is logged with the label
/// it claimed and the one it sniffed as, because that pair is what
/// identifies the broken provider.
pub(super) async fn cache_lyrics_bundle(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    file_hash: &str,
    bundle: PluginLyricsBundle,
    plugin_id: &str,
) -> AppResult<Option<LyricsPayload>> {
    let primary_format = plugin_format_to_app(bundle.primary.format);
    if !document_is_what_it_claims(&bundle.primary.content, primary_format) {
        tracing::warn!(
            plugin = %plugin_id,
            declared = ?primary_format,
            sniffed = ?detect_format(&bundle.primary.content),
            "plugin lyrics rejected: primary document is not the format it declared"
        );
        return Ok(None);
    }

    // One document per (kind, language), decided HERE rather than left to
    // the unique index. The insert below is `OR IGNORE`, so the database
    // would keep the first and drop the rest in silence — but the payload
    // returned to the caller is built from this list, so without the same
    // rule the panel would show two French translations now and one after
    // the next reload, with the cache and the response disagreeing about
    // what was fetched.
    //
    // NULL and "no language" are one slot, matching `COALESCE(language,
    // '')` in the index; otherwise two untagged pronunciations would both
    // pass here and only one would survive the write.
    let mut claimed: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let accepted: Vec<AssociatedLyrics> = bundle
        .associated
        .into_iter()
        .filter_map(|a| {
            let format = plugin_format_to_app(a.document.format);
            if !document_is_what_it_claims(&a.document.content, format) {
                tracing::warn!(
                    plugin = %plugin_id,
                    kind = ?a.kind,
                    declared = ?format,
                    sniffed = ?detect_format(&a.document.content),
                    "plugin lyrics: dropping an associated document that is not the format it declared"
                );
                return None;
            }
            let kind = match a.kind {
                PluginAssociatedKind::Translation => "translation".to_string(),
                PluginAssociatedKind::Pronunciation => "pronunciation".to_string(),
            };
            let slot = (
                kind.clone(),
                a.document.language.clone().unwrap_or_default(),
            );
            if !claimed.insert(slot) {
                tracing::warn!(
                    plugin = %plugin_id,
                    %kind,
                    language = ?a.document.language,
                    "plugin lyrics: dropping a second document for a slot already filled"
                );
                return None;
            }
            Some(AssociatedLyrics {
                kind,
                language: a.document.language,
                content: a.document.content,
                format,
            })
        })
        .collect();

    // A plugin id is not a `Provider` id, and both land in the same
    // column. Namespacing it keeps the two apart: `Provider::from_id`
    // can never accidentally match one, the badge can tell the user a
    // plugin answered, and `refetch_lyrics` knows to re-run the plugin
    // tier rather than reject the value as an unknown network provider.
    let provider_id = format!("{PLUGIN_PROVIDER_PREFIX}{plugin_id}");

    let now = now_ms();
    let mut tx = pool.begin().await?;

    write_primary_lyrics(
        &mut tx,
        file_hash,
        &bundle.primary.content,
        &primary_format,
        &LyricsSource::Api,
        Some(&provider_id),
        bundle.primary.language.as_deref(),
        now,
        None,
    )
    .await?;

    sqlx::query("DELETE FROM app.lyrics_associated WHERE file_hash = ?")
        .bind(file_hash)
        .execute(&mut *tx)
        .await?;

    for doc in &accepted {
        // A provider returning two documents for the same slot would
        // violate the unique index; the first wins and the rest are
        // skipped rather than failing the whole bundle. Inventing a
        // variant identity for rival versions can wait for a provider
        // that actually produces them.
        sqlx::query(
            "INSERT OR IGNORE INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(file_hash)
        .bind(&doc.kind)
        .bind(doc.language.as_deref())
        .bind(&doc.content)
        .bind(format_to_db(&doc.format))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    // This path writes its primary document itself rather than through
    // `upsert_lyrics`, so it carries the export too (#695).
    export_fetched_sidecar(
        pool,
        file_hash,
        &bundle.primary.content,
        &primary_format,
        &LyricsSource::Api,
    )
    .await;

    Ok(Some(LyricsPayload {
        track_id,
        content: bundle.primary.content,
        format: primary_format,
        source: LyricsSource::Api,
        provider: Some(provider_id),
        tag_write_skipped: None,
        associated: accepted,
    }))
}

/// Multi-tier lookup: cache → embedded tag → sidecar → enhanced/API
/// providers. Caches the first hit and returns it. Returns `None` if
/// local tiers fail and offline mode prevents network lookup.
/// How long one plugin gets to answer a lyrics lookup.
///
/// Matches the motion-artwork budget: the call is a network round-trip
/// inside a wasm guest, and the panel is waiting on it.
const LYRICS_PLUGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// What the plugin tier cached: the payload it returned, and the bundle
/// and plugin id it came from, kept so the same answer can be cached again.
pub(super) struct PluginAnswer {
    pub(super) payload: LyricsPayload,
    pub(super) bundle: PluginLyricsBundle,
    pub(super) plugin_id: String,
}

/// Which lyrics world a plugin declared: they are called differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LyricsWorld {
    /// `waveflow:metadata/v2` — `lyrics(artist, title)`.
    V2,
    /// `waveflow:metadata/v3` — `lyrics(track)`, with album, length and ISRC.
    V3,
}

/// The ISRC the track's file is tagged with, if any. The scanner keeps it
/// among the tags WaveFlow has no column for (`track_tag`, key `ISRC`
/// whatever the container). A read error is a missing ISRC: the lookup
/// still has the name to go on.
pub(super) async fn track_isrc(pool: &sqlx::SqlitePool, track_id: i64) -> Option<String> {
    match sqlx::query_scalar::<_, String>(
        "SELECT value FROM track_tag WHERE track_id = ? AND key = 'ISRC'",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    {
        Ok(isrc) => isrc
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        Err(err) => {
            tracing::debug!(%err, track_id, "could not read the track's ISRC");
            None
        }
    }
}

/// Ask every enabled `waveflow:metadata/v2` and `/v3` plugin for this
/// track, and cache the first bundle that survives validation.
///
/// Fan-out rather than a chain: plugins are independent, so they are all
/// asked at once and the first usable answer wins. A plugin that traps,
/// times out, or returns a document that is not the format it claimed is
/// skipped — one bad provider must not deny the tier.
///
/// `Ok(None)` means nothing was cached and the caller should carry on
/// down the waterfall. Nothing here writes a miss: a plugin having no
/// lyrics says nothing about whether LRCLIB does.
///
/// The answer carries the bundle it cached as well as the payload, so
/// `fetch_lyrics` can write it back after trying the network tiers for
/// something better (#668).
pub(super) async fn try_plugin_lyrics(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    only: Option<&str>,
) -> AppResult<Option<PluginAnswer>> {
    // Both lyrics worlds, each plugin carried with the one it declared: a
    // v2 plugin is asked about the track's name, a v3 one about the track.
    let mut plugins: Vec<(String, LyricsWorld)> = Vec::new();
    for (world, label) in [
        (LyricsWorld::V2, waveflow_core::plugin::worlds::METADATA_V2),
        (LyricsWorld::V3, waveflow_core::plugin::worlds::METADATA_V3),
    ] {
        for id in crate::commands::plugins::enabled_plugin_ids_providing(
            state,
            label,
            waveflow_core::plugin::manifest::functions::LYRICS,
        )
        .await?
        {
            plugins.push((id, world));
        }
    }
    // `refetch_lyrics` pins the plugin that answered last time. Filtering
    // the enumeration rather than taking the id on trust means a plugin
    // that has since been disabled or uninstalled simply drops out, and
    // the caller gets the same "nothing found" it would get for any other
    // silent provider — no separate not-installed path to keep correct.
    if let Some(wanted) = only {
        plugins.retain(|(id, _)| id == wanted);
    }
    if plugins.is_empty() {
        return Ok(None);
    }

    let query = waveflow_core::plugin::runtime::TrackQuery {
        artist: meta.artist_name.clone().unwrap_or_default(),
        title: meta.title.clone(),
        album: meta.album_title.clone().filter(|a| !a.trim().is_empty()),
        duration_ms: u32::try_from(meta.duration_ms).ok().filter(|ms| *ms > 0),
        // Read only when a v3 plugin will see it.
        isrc: if plugins.iter().any(|(_, world)| *world == LyricsWorld::V3) {
            track_isrc(pool, track_id).await
        } else {
            None
        },
    };

    let mut set = tokio::task::JoinSet::new();
    for (plugin_id, world) in plugins {
        // Take the lock HANDLE here (a fast map op) and acquire the guard
        // inside the blocking closure, so it spans the real work: the
        // guest call is uncancellable, and an early drop on timeout would
        // otherwise let an enable/uninstall race a call still running.
        let lock_arc = crate::commands::plugins::plugin_lock_arc(state, &plugin_id).await;
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let query = query.clone();

        set.spawn(async move {
            let outcome = tokio::time::timeout(
                LYRICS_PLUGIN_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    let _guard = lock_arc.blocking_lock_owned();
                    match world {
                        LyricsWorld::V2 => waveflow_core::plugin::runtime::metadata_v2_lyrics(
                            &runtime,
                            &paths,
                            &id_owned,
                            &query.artist,
                            &query.title,
                        ),
                        LyricsWorld::V3 => waveflow_core::plugin::runtime::metadata_v3_lyrics(
                            &runtime, &paths, &id_owned, &query,
                        ),
                    }
                }),
            )
            .await;
            (plugin_id, outcome)
        });
    }

    while let Some(joined) = set.join_next().await {
        let (plugin_id, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%e, "plugin lyrics task join failed; skipping");
                continue;
            }
        };
        let bundle = match outcome {
            Ok(Ok(Ok(Some(bundle)))) => bundle,
            // The plugin answered and has nothing for this track.
            Ok(Ok(Ok(None))) => continue,
            Ok(Ok(Err(err))) if err.is_load_failure() => {
                tracing::warn!(
                    plugin = %plugin_id,
                    err = %err.detail(),
                    "plugin lyrics: the plugin could not be loaded"
                );
                continue;
            }
            Ok(Ok(Err(err))) => {
                if !crate::plugin_attention::inspect(&plugin_id, &err) {
                    tracing::debug!(
                        plugin = %plugin_id,
                        err = %err.detail(),
                        "plugin lyrics lookup failed"
                    );
                }
                continue;
            }
            Ok(Err(e)) => {
                tracing::warn!(plugin = %plugin_id, %e, "plugin lyrics task panicked");
                continue;
            }
            Err(_) => {
                tracing::warn!(plugin = %plugin_id, "plugin lyrics lookup timed out");
                continue;
            }
        };

        if let Some(payload) =
            cache_lyrics_bundle(pool, track_id, &meta.file_hash, bundle.clone(), &plugin_id).await?
        {
            return Ok(Some(PluginAnswer {
                payload,
                bundle,
                plugin_id,
            }));
        }
        // Validation rejected it; `cache_lyrics_bundle` has already said
        // why. Keep asking the others.
    }

    Ok(None)
}
