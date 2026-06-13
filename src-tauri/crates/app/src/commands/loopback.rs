//! Shared helpers for the loopback OAuth callback flows.
//!
//! Both [`server_auth`](crate::commands::server_auth) (Better Auth
//! handshake against `waveflow-server`) and [`spotify`](crate::commands::spotify)
//! spin up a one-shot `tiny_http` server on a local port to capture
//! the redirect from the system browser. Each one renders a small
//! HTML confirmation page so the user knows it's safe to close the
//! tab. The plumbing diverges (different callback URLs, different
//! query-string shapes, different state validation), but the
//! "render a `text/html` response" step is identical — extracting it
//! here keeps a single source of truth and makes sure a future fix
//! ships to both flows at once.

use tiny_http::{Header, Response};

/// Wrap a static HTML body in a `tiny_http` response with the right
/// `Content-Type` header.
///
/// `tiny_http::Response::from_string` does NOT stamp a Content-Type
/// of its own; the receiving browser falls back to `text/plain` and
/// renders the raw HTML markup as literal text. That broke both the
/// Better Auth and Spotify confirmation pages until this helper
/// landed.
pub(crate) fn html_response(body: &'static str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body).with_header(
        Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("static html content-type header is well-formed"),
    )
}
