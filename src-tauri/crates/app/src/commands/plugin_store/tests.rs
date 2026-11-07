use super::*;

/// Decode a catalogue the way [`fetch_registry`] does, then fold
/// the sibling in the way [`list_plugin_marketplace`] does — the
/// one place where a published registry meets the store row.
fn resolve_description(json: &str, lang: &str) -> Option<String> {
    let reg: Registry = serde_json::from_str(json).expect("registry decodes");
    let e = reg.plugins.into_iter().next().expect("one entry");
    let merged = e.description.merged_with(e.description_i18n);
    merged.resolve(lang).map(str::to_string)
}

fn registry_json(description_field: &str) -> String {
    format!(
        r#"{{
              "schema_version": 1,
              "plugins": [{{
                "id": "apple-artwork",
                "name": "Apple Motion Artwork",
                {description_field},
                "author": "InstaZDLL",
                "repo": "InstaZDLL/waveflow-plugin-apple-artwork",
                "world": "waveflow:metadata/v1",
                "version": "0.3.0",
                "blake3": "3a369e9f03bc7679ca86973f29ce78a8243db1f54ce41cbd936af734318e7e51",
                "permissions": {{ "http": [], "storage_read": false, "storage_state": true }}
              }}]
            }}"#
    )
}

/// The shape every published registry uses: a plain string plus a
/// sibling map. This is the compatibility contract the whole
/// feature rests on, so it gets its own test at the seam.
#[test]
fn registry_entry_merges_its_i18n_sibling() {
    let json = registry_json(
        r#""description": "Animated album covers.",
               "description_i18n": { "fr": "Pochettes animées." }"#,
    );
    assert_eq!(
        resolve_description(&json, "fr").as_deref(),
        Some("Pochettes animées.")
    );
    // The plain string takes the `en` slot, so untranslated
    // languages still read the author's original text.
    assert_eq!(
        resolve_description(&json, "en").as_deref(),
        Some("Animated album covers.")
    );
    assert_eq!(
        resolve_description(&json, "ja").as_deref(),
        Some("Animated album covers.")
    );
}

/// Every entry published to date, and the only form older clients
/// can read — it must survive the merge untouched.
#[test]
fn registry_entry_without_a_sibling_is_unchanged() {
    let json = registry_json(r#""description": "Animated album covers.""#);
    for lang in ["en", "fr", "zh-CN"] {
        assert_eq!(
            resolve_description(&json, lang).as_deref(),
            Some("Animated album covers.")
        );
    }
}

/// A hostile or malformed catalogue can carry an unrenderable
/// description; the store row must degrade to "no description"
/// rather than an empty paragraph holding its margin.
#[test]
fn registry_entry_with_nothing_renderable_resolves_to_none() {
    let json = registry_json(r#""description": "", "description_i18n": { "fr": "  " }"#);
    assert_eq!(resolve_description(&json, "fr"), None);
}

// ----- sources and the mirror ------------------------------------------------

fn entry(id: &str, version: &str, download_url: Option<&str>) -> RegistryEntry {
    let json = serde_json::json!({
        "id": id,
        "name": id,
        "description": "d",
        "author": "a",
        "repo": "InstaZDLL/waveflow-plugin-x",
        "world": "waveflow:metadata/v3",
        "version": version,
        "blake3": "00",
        "download_url": download_url,
        "permissions": {},
    });
    serde_json::from_value(json).expect("entry decodes")
}

/// The R2 copy sits between waveflow.app and GitHub: it is the only other
/// copy that lists the private plugins, so it must be asked before the
/// public-only ones.
#[test]
fn the_mirror_registry_is_read_right_after_waveflow_app() {
    assert_eq!(
        REGISTRY_URLS[0],
        "https://waveflow.app/api/plugins/registry"
    );
    assert_eq!(
        REGISTRY_URLS[1],
        format!("{PLUGIN_MIRROR_BASE}/registry.json")
    );
    for url in &REGISTRY_URLS[2..] {
        assert!(!url.contains("waveflow.app"), "{url}");
    }
    assert!(PLUGIN_MIRROR_BASE.starts_with("https://download.waveflow.app/"));
}

#[test]
fn the_mirror_archive_url_is_what_the_staging_script_writes() {
    assert_eq!(
        mirror_archive_url("apple-lyrics", "0.1.3").as_deref(),
        Some("https://download.waveflow.app/plugins/apple-lyrics/apple-lyrics-v0.1.3.zip")
    );
    assert_eq!(
        mirror_archive_url("x", "1.0.0-beta.1+build.7").as_deref(),
        Some("https://download.waveflow.app/plugins/x/x-v1.0.0-beta.1+build.7.zip")
    );
}

#[test]
fn a_name_that_could_leave_its_folder_has_no_mirror_copy() {
    for (id, version) in [
        ("x", "../../registry"),
        ("x", "1/2"),
        ("x", ""),
        ("x", ".1"),
        ("x", "1?a=b"),
        ("../x", "1.0.0"),
        ("X", "1.0.0"),
        ("", "1.0.0"),
    ] {
        assert_eq!(mirror_archive_url(id, version), None, "{id:?} {version:?}");
    }
}

#[test]
fn a_github_release_falls_back_to_the_mirror() {
    let e = entry("release-radar", "0.1.3", None);
    assert_eq!(
        archive_sources(&e).unwrap(),
        [
            "https://github.com/InstaZDLL/waveflow-plugin-x/releases/download/v0.1.3/release-radar-v0.1.3.zip",
            "https://download.waveflow.app/plugins/release-radar/release-radar-v0.1.3.zip",
        ]
    );
}

#[test]
fn a_download_url_comes_first_then_the_mirror() {
    let direct = "https://waveflow.app/plugins/apple-lyrics/apple-lyrics-v0.1.3.zip";
    let e = entry("apple-lyrics", "0.1.3", Some(direct));
    assert_eq!(
        archive_sources(&e).unwrap(),
        [
            direct,
            "https://download.waveflow.app/plugins/apple-lyrics/apple-lyrics-v0.1.3.zip",
        ]
    );
}

#[test]
fn the_mirror_is_not_asked_twice() {
    let mirror = "https://download.waveflow.app/plugins/x/x-v1.0.0.zip";
    let e = entry("x", "1.0.0", Some(mirror));
    assert_eq!(archive_sources(&e).unwrap(), [mirror]);
}

#[test]
fn a_version_with_no_mirror_copy_keeps_only_its_own_address() {
    let e = entry("x", "1/2", Some("https://cdn.example/x.zip"));
    assert_eq!(archive_sources(&e).unwrap(), ["https://cdn.example/x.zip"]);
}

/// An unsafe `download_url` is a broken registry, not an unreachable host:
/// refused, never quietly routed to the mirror.
#[test]
fn an_unsafe_download_url_is_refused_not_routed_to_the_mirror() {
    for url in ["http://cdn.example/x.zip", "https://127.0.0.1/x.zip"] {
        let e = entry("x", "1.0.0", Some(url));
        assert!(archive_sources(&e).is_err(), "{url}");
    }
}

// ----- the fallback, against a local server ----------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One canned answer per path, raw bytes, the listener alive until the test
/// ends. Paths not listed answer 404.
async fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let path = path.split('?').next().unwrap_or("/").to_string();
                let answer = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, a)| a.clone())
                    .unwrap_or_else(|| NOT_FOUND.to_vec());
                let _ = socket.write_all(&answer).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

const NOT_FOUND: &[u8] = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";

fn ok_with(body: &[u8]) -> Vec<u8> {
    let mut answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    answer.extend_from_slice(body);
    answer
}

fn plugin_zip(wasm: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("plugin.wasm", options).unwrap();
    zip.write_all(wasm).unwrap();
    zip.start_file("manifest.toml", options).unwrap();
    zip.write_all(b"schema_version = 1\n").unwrap();
    zip.finish().unwrap().into_inner()
}

/// Each way the first host can fail to deliver, and the mirror's copy is
/// what comes back.
#[tokio::test]
async fn a_failed_download_falls_back_to_the_next_source() {
    let good = plugin_zip(b"\0asm good");
    let mut truncated =
        b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n".to_vec();
    truncated.extend_from_slice(&good[..10]);
    let base = serve(vec![
        ("/missing", NOT_FOUND.to_vec()),
        (
            "/error",
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ),
        ("/portal", ok_with(b"<html>account suspended</html>")),
        ("/truncated", truncated),
        ("/mirror", ok_with(&good)),
    ])
    .await;
    let client = reqwest::Client::new();
    for first in ["/missing", "/error", "/portal", "/truncated"] {
        let sources = [format!("{base}{first}"), format!("{base}/mirror")];
        let bytes = download_first(&client, &sources)
            .await
            .unwrap_or_else(|e| panic!("{first}: {e}"));
        assert_eq!(bytes, good, "{first}");
    }

    // A refused connection, the plain network error.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let sources = [
        format!("http://127.0.0.1:{closed}/x.zip"),
        format!("{base}/mirror"),
    ];
    assert_eq!(download_first(&client, &sources).await.unwrap(), good);
}

#[tokio::test]
async fn the_first_source_that_answers_is_the_only_one_asked() {
    let first = plugin_zip(b"\0asm first");
    let base = serve(vec![
        ("/first", ok_with(&first)),
        ("/mirror", ok_with(&plugin_zip(b"\0asm mirror"))),
    ])
    .await;
    let sources = [format!("{base}/first"), format!("{base}/mirror")];
    let bytes = download_first(&reqwest::Client::new(), &sources)
        .await
        .unwrap();
    assert_eq!(bytes, first);
}

/// Both hosts failing is one error naming both, and the signed query of
/// neither.
#[tokio::test]
async fn every_source_failing_names_each_failure_without_its_query() {
    let base = serve(vec![]).await;
    let sources = [
        format!("{base}/a.zip?token=s3cret"),
        format!("{base}/b.zip?token=s3cret"),
    ];
    let err = download_first(&reqwest::Client::new(), &sources)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("/a.zip") && err.contains("/b.zip"), "{err}");
    assert_eq!(err.matches("HTTP 404").count(), 2, "{err}");
    assert!(!err.contains("s3cret"), "{err}");
}

/// A body past the cap is refused before it is read, from either host.
#[tokio::test]
async fn an_oversized_body_is_a_failed_download() {
    let mut huge = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_DOWNLOAD_BYTES + 1
    )
    .into_bytes();
    huge.extend_from_slice(b"PK");
    let base = serve(vec![("/huge", huge)]).await;
    let err = download_first(&reqwest::Client::new(), &[format!("{base}/huge")])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("more than"), "{err}");
}

/// The fallback is for a download that fails, not for one whose bytes the
/// registry does not pin: the first host's archive is taken, and the
/// install refuses it — the mirror's (matching) copy is never fetched.
#[tokio::test]
async fn a_blake3_mismatch_refuses_without_trying_the_mirror() {
    let pinned = blake3::hash(b"\0asm pinned").to_hex().to_string();
    let base = serve(vec![
        ("/tampered", ok_with(&plugin_zip(b"\0asm tampered"))),
        ("/mirror", ok_with(&plugin_zip(b"\0asm pinned"))),
    ])
    .await;
    let sources = [format!("{base}/tampered"), format!("{base}/mirror")];
    let bytes = download_first(&reqwest::Client::new(), &sources)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let paths = PluginPaths::from_app_data(dir.path());
    let err = install_from_zip_bytes(&paths, "x", &bytes, &pinned, "1.0.0", "w")
        .unwrap_err()
        .to_string();
    assert!(err.contains("blake3 mismatch"), "{err}");
}

/// Offline mode is asked before every host, not once per install: switched
/// on mid-install, it stops the next request.
#[tokio::test]
async fn offline_mode_stops_before_any_source() {
    let base = serve(vec![("/mirror", ok_with(&plugin_zip(b"\0asm")))]).await;
    let _offline = crate::offline::scoped_to_this_thread(true);
    let err = download_first(&reqwest::Client::new(), &[format!("{base}/mirror")])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("offline"), "{err}");
}
