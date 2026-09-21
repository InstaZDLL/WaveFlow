//! Which online lyrics providers are asked, and for which tracks.
//!
//! Three rules decide it, all read by [`super::lyrics`] before a network
//! lookup and none of them touching the local tiers (embedded tag,
//! sidecar, description), which cost nothing and belong to the file:
//!
//! - **Providers the user switched off** are never asked (#722).
//! - **Tracks in an excluded genre** skip the online search (#721).
//! - **A provider that just failed to connect** is left out for a while
//!   ([`record_failure`], #720), so a host that is down costs its
//!   connect timeout once per cooldown rather than on every track.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use waveflow_syncedlyrics::{Error as ProviderError, Provider};

use crate::error::AppResult;

/// `profile_setting` key: JSON array of provider ids (`Provider::as_str`)
/// the query chain must not ask. Absent → [`DEFAULT_DISABLED`].
pub const DISABLED_PROVIDERS_KEY: &str = "lyrics.disabled_providers";

/// `profile_setting` key: JSON array of genre names whose tracks skip the
/// online lyrics search. Absent → [`DEFAULT_EXCLUDED_GENRES`].
pub const EXCLUDED_GENRES_KEY: &str = "lyrics.excluded_genres";

/// Off until someone turns it on. Genius cannot answer a normal install:
/// its search endpoint refuses a request without a session cookie (403),
/// and the cookie is read from an environment variable nothing sets. When
/// it does answer, it has no timestamps — only plain text, with its
/// section markers (`[Chorus]`) kept in.
const DEFAULT_DISABLED: &[Provider] = &[Provider::Genius];

/// These two essentially never carry lyrics. Matched by
/// [`genre_matches`], so `Instrumentals`, `Instrumental Hip-Hop`, `Lofi`,
/// `Lo Fi` and `lo-fi hip hop` are all caught without being listed.
const DEFAULT_EXCLUDED_GENRES: &[&str] = &["Instrumental", "Lo-fi"];

/// The providers of the query-based chain, in the order they are asked.
///
/// LRCLIB leads even though the exact-match tier already asked it, because
/// the two ask *differently*: that tier hits `/api/get`, which matches on
/// artist + track + album + duration and 404s when any of them disagrees
/// with the file's tags (a remaster, a "Deluxe" album name, a rip a few
/// seconds off). This chain goes through the provider's `/api/search`,
/// which is fuzzy. Without it, that 404 dropped straight to providers
/// that answer for almost anything — so a track LRCLIB *does* carry came
/// back from Genius (#463).
///
/// **Musixmatch is intentionally absent.** It has its own dedicated tier
/// (the word-level lookup, behind its own opt-in); listing it here would
/// issue a second, identical request for every miss.
pub const CHAIN: [Provider; 4] = [
    Provider::Lrclib,
    Provider::NetEase,
    Provider::Megalobiz,
    Provider::Genius,
];

/// Whether the user can switch this provider off. LRCLIB cannot: it is
/// also the exact-match tier, whose "instrumental" verdict and curated
/// synced lyrics the rest of the waterfall is built around, and a chain
/// without it is the #463 bug by choice. Musixmatch has its own opt-in.
pub fn is_switchable(provider: Provider) -> bool {
    matches!(
        provider,
        Provider::NetEase | Provider::Megalobiz | Provider::Genius
    )
}

async fn read_setting(pool: &sqlx::SqlitePool, key: &str) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Parse a stored JSON array of strings. `None` for anything else, so a
/// damaged value falls back to the default instead of silently meaning
/// "nothing" — for the genres, that would re-open the network to every
/// instrumental in the library.
fn parse_string_list(raw: &str) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(raw).ok()
}

fn disabled_from(raw: Option<&str>) -> Vec<Provider> {
    let ids = match raw.map(parse_string_list) {
        None => return DEFAULT_DISABLED.to_vec(),
        Some(Some(ids)) => ids,
        Some(None) => {
            tracing::warn!("unreadable {DISABLED_PROVIDERS_KEY}; using the default");
            return DEFAULT_DISABLED.to_vec();
        }
    };
    ids.iter()
        .filter_map(|id| Provider::from_id(id))
        .filter(|p| is_switchable(*p))
        .collect()
}

/// The query chain as this profile has configured it: [`CHAIN`] minus the
/// providers the user switched off, order unchanged.
pub async fn enabled_chain(pool: &sqlx::SqlitePool) -> AppResult<Vec<Provider>> {
    let raw = read_setting(pool, DISABLED_PROVIDERS_KEY).await?;
    let disabled = disabled_from(raw.as_deref());
    Ok(CHAIN
        .into_iter()
        .filter(|p| !disabled.contains(p))
        .collect())
}

fn excluded_from(raw: Option<&str>) -> Vec<String> {
    match raw.map(parse_string_list) {
        None => DEFAULT_EXCLUDED_GENRES
            .iter()
            .map(|g| g.to_string())
            .collect(),
        Some(Some(genres)) => genres,
        Some(None) => {
            tracing::warn!("unreadable {EXCLUDED_GENRES_KEY}; using the default");
            DEFAULT_EXCLUDED_GENRES
                .iter()
                .map(|g| g.to_string())
                .collect()
        }
    }
}

/// The genre patterns this profile excludes from the online search.
pub async fn excluded_genres(pool: &sqlx::SqlitePool) -> AppResult<Vec<String>> {
    let raw = read_setting(pool, EXCLUDED_GENRES_KEY).await?;
    Ok(excluded_from(raw.as_deref()))
}

/// Split a genre into lowercase alphanumeric words. Everything else —
/// spaces, hyphens, slashes, dots — is a separator, which is what makes
/// `Lo-Fi`, `Lo Fi` and `lo.fi` the same two words.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether `genre` is a variant of `pattern`.
///
/// It is, when some run of consecutive words in the genre, glued
/// together, spells the pattern with its separators removed — or that
/// plus a plural `s`. So `lo-fi` matches `Lofi`, `Lo Fi` and
/// `lo-fi hip hop`, and `instrumental` matches `Instrumentals` and
/// `Instrumental Hip-Hop`.
///
/// Whole words only: a substring test would let a user's `rap` exclude
/// every `Trap` track, and a genre is a label the user did not write.
pub fn genre_matches(genre: &str, pattern: &str) -> bool {
    let target: String = words(pattern).concat();
    if target.is_empty() {
        return false;
    }
    let plural = format!("{target}s");
    let words = words(genre);
    for start in 0..words.len() {
        let mut glued = String::new();
        for word in &words[start..] {
            glued.push_str(word);
            if glued == target || glued == plural {
                return true;
            }
            if glued.len() > plural.len() {
                break;
            }
        }
    }
    false
}

/// Whether any of `genres` matches any of `patterns`. A track has several
/// genres (`track_genre` is many-to-many), and one match is enough.
pub fn any_genre_excluded<'a>(
    genres: impl IntoIterator<Item = &'a str>,
    patterns: &[String],
) -> bool {
    genres
        .into_iter()
        .any(|g| patterns.iter().any(|p| genre_matches(g, p)))
}

/// The first of `genres` that matches one of `patterns`, as the track
/// spells it — what the lyrics panel names when it explains why nothing
/// was searched.
pub fn first_excluded_genre<'a>(
    genres: impl IntoIterator<Item = &'a str>,
    patterns: &[String],
) -> Option<&'a str> {
    genres
        .into_iter()
        .find(|g| patterns.iter().any(|p| genre_matches(g, p)))
}

/// Whether this track's online lyrics search is skipped for its genre.
pub async fn track_is_excluded(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<bool> {
    Ok(excluded_genre_of(pool, track_id).await?.is_some())
}

/// The genre that keeps this track out of the online lyrics search, if
/// one does.
pub async fn excluded_genre_of(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Option<String>> {
    let patterns = excluded_genres(pool).await?;
    if patterns.is_empty() {
        return Ok(None);
    }
    let genres: Vec<String> = sqlx::query_scalar(
        "SELECT g.name
           FROM track_genre tg
           JOIN genre g ON g.id = tg.genre_id
          WHERE tg.track_id = ?",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(first_excluded_genre(genres.iter().map(String::as_str), &patterns).map(str::to_owned))
}

/// How long a provider that could not be reached is left out of the
/// automatic lookups before it is tried again.
const COOLDOWN: Duration = Duration::from_secs(10 * 60);

#[derive(Default, Clone, Copy)]
struct Health {
    down_until: Option<Instant>,
    warned: bool,
}

/// Process-wide record of which providers are failing (#720).
///
/// Two jobs. **Cooldown**: a provider that failed to connect or refused
/// the request is left out of the automatic lookups for [`COOLDOWN`]. A
/// host that is down otherwise costs its full connect timeout on every
/// track — seconds per panel open, hours over a library prefetch.
/// **Visibility**: the first failure of each provider in a session is
/// logged at `WARN`, so the log file (INFO and above) names the provider
/// that is down; the rest go to `debug`, so one dead host does not fill
/// the file line by line.
///
/// A lookup that leaves a provider out is a *partial* miss, never a clean
/// one: the provider was not heard, so the miss is cached to expire.
static HEALTH: Mutex<[Health; 5]> = Mutex::new(
    [Health {
        down_until: None,
        warned: false,
    }; 5],
);

fn slot(provider: Provider) -> usize {
    match provider {
        Provider::Musixmatch => 0,
        Provider::Lrclib => 1,
        Provider::NetEase => 2,
        Provider::Megalobiz => 3,
        Provider::Genius => 4,
    }
}

/// Record that `provider` failed to answer.
pub fn record_failure(provider: Provider, error: &ProviderError) {
    let Ok(mut health) = HEALTH.lock() else {
        return;
    };
    let entry = &mut health[slot(provider)];
    if error.is_transport() {
        entry.down_until = Some(Instant::now() + COOLDOWN);
    }
    if entry.warned {
        tracing::debug!(provider = provider.as_str(), %error, "lyrics provider failed");
    } else {
        entry.warned = true;
        tracing::warn!(
            provider = provider.as_str(),
            %error,
            "lyrics provider failed; its later failures this session are logged at debug"
        );
    }
}

/// Record that `provider` answered, which ends any cooldown.
pub fn record_answer(provider: Provider) {
    if let Ok(mut health) = HEALTH.lock() {
        health[slot(provider)].down_until = None;
    }
}

/// Whether `provider` is sitting out a cooldown.
pub fn is_cooling_down(provider: Provider) -> bool {
    let Ok(health) = HEALTH.lock() else {
        return false;
    };
    health[slot(provider)]
        .down_until
        .is_some_and(|until| Instant::now() < until)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel names the genre the way the track carries it, not the
    /// pattern it matched: "Lo-Fi Hip Hop", not "lo-fi".
    #[test]
    fn the_excluded_genre_is_named_as_tagged() {
        let defaults: Vec<String> = excluded_from(None);
        assert_eq!(
            first_excluded_genre(["Jazz", "Lo-Fi Hip Hop"], &defaults),
            Some("Lo-Fi Hip Hop")
        );
        assert_eq!(first_excluded_genre(["Jazz", "Trap"], &defaults), None);
    }

    #[test]
    fn the_default_genres_catch_their_variants() {
        let defaults: Vec<String> = excluded_from(None);
        for genre in [
            "Instrumental",
            "instrumentals",
            "Instrumental Hip-Hop",
            "Lo-Fi",
            "Lofi",
            "Lo Fi",
            "lo-fi hip hop",
            "LOFI BEATS",
            "Chillhop / Lo-fi",
        ] {
            assert!(
                any_genre_excluded([genre], &defaults),
                "{genre} should be excluded by default"
            );
        }
    }

    #[test]
    fn a_pattern_matches_whole_words_only() {
        // A substring test would exclude every trap track for "rap".
        assert!(!genre_matches("Trap", "rap"));
        assert!(!genre_matches("Folk", "lofi"));
        assert!(!genre_matches("Instrumentalism", "instrumental"));
        assert!(genre_matches("French Rap", "rap"));
        assert!(!any_genre_excluded(
            ["Pop", "Hip-Hop"],
            &excluded_from(None)
        ));
    }

    #[test]
    fn a_blank_pattern_excludes_nothing() {
        assert!(!genre_matches("Pop", ""));
        assert!(!genre_matches("Pop", " - "));
    }

    #[test]
    fn an_empty_list_is_a_choice_but_junk_is_not() {
        assert!(excluded_from(Some("[]")).is_empty());
        assert_eq!(excluded_from(Some("not json")), excluded_from(None));
        assert!(disabled_from(Some("[]")).is_empty());
        assert_eq!(disabled_from(Some("{")), DEFAULT_DISABLED.to_vec());
    }

    #[test]
    fn genius_is_off_until_turned_on() {
        assert_eq!(disabled_from(None), vec![Provider::Genius]);
    }

    #[test]
    fn lrclib_cannot_be_switched_off() {
        let disabled = disabled_from(Some(r#"["lrclib","megalobiz","nope"]"#));
        assert_eq!(disabled, vec![Provider::Megalobiz]);
    }
}
