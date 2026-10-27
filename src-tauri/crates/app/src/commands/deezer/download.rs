//! Downloading an image from a URL the user gave, refusing any host
//! that is not public.

use waveflow_core::error::describe_http_error;

use crate::error::{AppError, AppResult};

/// Whether a URL is one we are willing to fetch on a third party's say-so.
///
/// `http`/`https` only, and never a host that sits inside the machine or
/// the local network: loopback, link-local, and the private ranges. A
/// hostname is accepted as-is — resolving it here would move the check
/// earlier without making it sound, since DNS can answer differently at
/// connect time. The redirect policy applies the same rule to each hop,
/// which is where a public host would otherwise send us.
pub(crate) fn is_public_http_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    match parsed.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            // A name with no dot (`router`, `nas`) is resolved on the
            // local network by definition, and so are the suffixes home
            // routers and companies hand out — recognisable without DNS,
            // like `.local`.
            name.contains('.')
                && name != "localhost"
                && ![
                    ".localhost",
                    ".local",
                    ".lan",
                    ".home",
                    ".home.arpa",
                    ".internal",
                    ".intranet",
                    ".localdomain",
                    ".corp",
                ]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        }
        Some(url::Host::Ipv4(ip)) => is_public_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => {
            // `::ffff:192.168.1.1` is the LAN address in IPv6 clothing:
            // judged as the IPv4 address it carries.
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public_ipv4(v4);
            }
            // `is_unique_local` / `is_unicast_link_local` are unstable,
            // so the two ranges are matched by prefix: fc00::/7 and
            // fe80::/10.
            let segments = ip.segments();
            !(ip.is_loopback()
                || ip.is_unspecified()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80)
        }
        None => false,
    }
}

/// The IPv4 half of [`is_public_http_url`]: not loopback, private,
/// link-local, unspecified or broadcast, and outside `0.0.0.0/8` ("this
/// network") and the `100.64.0.0/10` carrier-grade NAT range, which sit
/// inside a provider's network rather than on the internet.
fn is_public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || a == 0
        || (a == 100 && (b & 0xc0) == 64))
}

/// Hard cap on a single image download. Deezer's `picture_xl` /
/// `cover_xl` payloads top out around 200 KB; 10 MiB is generous
/// headroom while still guarding against a hostile (or compromised)
/// remote that streams unbounded data into our process memory.
pub(crate) const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

pub(super) async fn download_image_bytes(url: &str) -> AppResult<Vec<u8>> {
    // Every URL here is named by a third party -- Deezer's API, or
    // TheAudioDB's list of fanarts -- and is fetched by a process
    // sitting inside the user's network. A URL pointing at localhost or
    // at a LAN address would turn this downloader into a probe of
    // machines that third party cannot otherwise reach. Checked on the
    // way in, and again on every redirect hop, because a public host is
    // free to answer with a redirect to a private one.
    if !is_public_http_url(url) {
        return Err(AppError::Other("refusing to fetch a non-public URL".into()));
    }
    let client = reqwest::Client::builder()
        .user_agent("WaveFlow/0.1")
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if is_public_http_url(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|err| AppError::Other(format!("http client build failed: {err}")))?;

    let mut resp = client.get(url).send().await.map_err(|err| {
        AppError::Other(format!("download failed: {}", describe_http_error(&err)))
    })?;
    if !resp.status().is_success() {
        return Err(AppError::Other(format!(
            "download status {}",
            resp.status()
        )));
    }

    // Early-reject when the server is honest about its size — saves us
    // pulling a single chunk we'd throw away anyway.
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_IMAGE_BYTES {
            return Err(AppError::Other(format!(
                "image too large ({} bytes, max {})",
                len, MAX_IMAGE_BYTES,
            )));
        }
    }

    // Pull chunk-by-chunk via `Response::chunk()` (built-in, no need
    // for the `stream` feature on reqwest) so a server lying about
    // Content-Length — or chunked transfer with no length at all —
    // still can't OOM us.
    let mut bytes = Vec::with_capacity(64 * 1024);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|err| AppError::Other(format!("read failed: {}", describe_http_error(&err))))?
    {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            return Err(AppError::Other(format!(
                "image exceeds max size ({} bytes)",
                MAX_IMAGE_BYTES,
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
