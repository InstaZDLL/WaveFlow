//! KDC (Key Distribution Center) transport client.
//!
//! Sends AS-REQ and TGS-REQ messages to a Kerberos KDC on port 88.
//! Tries UDP first (no framing), falls back to TCP (4-byte big-endian
//! length prefix) when the response indicates KRB_ERR_RESPONSE_TOO_BIG
//! (error code 52).
//!
//! Transport details per RFC 4120 section 7.2 and MS-KILE section 2.1:
//! - UDP: raw DER bytes, no length prefix, max 65535 bytes
//! - TCP: 4-byte big-endian length prefix, then DER bytes
//! - Retry: up to 3 attempts with exponential backoff (1s, 2s, 4s)
//!
//! The functions here are transport-only: they send raw bytes and return
//! raw bytes. No ASN.1 parsing beyond detecting error code 52 in the
//! UDP-to-TCP fallback path.

use log::{debug, trace, warn};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::rt::net::{TcpStream, UdpSocket};
use crate::rt::{sleep, timeout as with_timeout};

/// Default Kerberos port (RFC 4120).
const KERBEROS_PORT: u16 = 88;

/// Maximum UDP receive buffer size.
const UDP_MAX_SIZE: usize = 65535;

/// KRB_ERR_RESPONSE_TOO_BIG error code (RFC 4120 section 7.2.1).
const KRB_ERR_RESPONSE_TOO_BIG: u32 = 52;

/// Maximum TCP frame size we accept (1 MB, generous for Kerberos).
const MAX_KDC_FRAME_SIZE: usize = 1024 * 1024;

/// Number of retry attempts per transport.
const MAX_RETRIES: u32 = 3;

/// Base retry delay (doubles each attempt).
const RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// Configuration for connecting to a KDC.
#[derive(Debug, Clone)]
pub struct KdcConfig {
    /// KDC address (host:port or just host, defaults to port 88).
    pub address: String,
    /// Connection/request timeout.
    pub timeout: Duration,
}

/// Resolve the KDC address to include a port if not specified.
fn resolve_address(address: &str) -> String {
    if address.contains(':') {
        address.to_string()
    } else {
        format!("{}:{}", address, KERBEROS_PORT)
    }
}

/// Send a Kerberos message to the KDC and receive the response.
///
/// Tries UDP first. If the response indicates the message was too
/// large for UDP (KRB_ERR_RESPONSE_TOO_BIG), retries with TCP.
///
/// UDP framing: raw DER bytes, no length prefix.
/// TCP framing: 4-byte big-endian length prefix, then DER bytes.
pub async fn send_to_kdc(config: &KdcConfig, message: &[u8]) -> Result<Vec<u8>> {
    let addr = resolve_address(&config.address);
    trace!("kdc: sending {} bytes addr={:?}", message.len(), addr);

    // Try UDP first.
    match send_udp(&addr, message, config.timeout).await {
        Ok(response) => {
            if is_response_too_big(&response) {
                debug!("kdc: got KRB_ERR_RESPONSE_TOO_BIG, retrying with TCP");
                send_tcp(&addr, message, config.timeout).await
            } else {
                Ok(response)
            }
        }
        Err(e) => {
            warn!("kdc: UDP failed ({}), falling back to TCP", e);
            send_tcp(&addr, message, config.timeout).await
        }
    }
}

/// Send a Kerberos message via UDP.
async fn send_udp(addr: &str, message: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .await
        .map_err(Error::Io)?;

    let mut last_err = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
            debug!("kdc: UDP retry {} after {:?}", attempt, delay);
            sleep(delay).await;
        }

        // Send the raw DER bytes (no framing for UDP).
        match with_timeout(timeout, socket.send_to(message, addr)).await {
            Ok(Ok(n)) => {
                trace!("kdc: UDP sent {} bytes", n);
            }
            Ok(Err(e)) => {
                last_err = Some(Error::Io(e));
                continue;
            }
            Err(_) => {
                last_err = Some(Error::Timeout);
                continue;
            }
        }

        // Receive the response.
        let mut buf = vec![0u8; UDP_MAX_SIZE];
        match with_timeout(timeout, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, _src))) => {
                trace!("kdc: UDP received {} bytes", n);
                buf.truncate(n);
                return Ok(buf);
            }
            Ok(Err(e)) => {
                last_err = Some(Error::Io(e));
            }
            Err(_) => {
                last_err = Some(Error::Timeout);
            }
        }
    }

    Err(last_err.unwrap_or(Error::Timeout))
}

/// Send a Kerberos message via TCP.
async fn send_tcp(addr: &str, message: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let mut last_err = None;

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_BASE_DELAY * 2u32.pow(attempt - 1);
            debug!("kdc: TCP retry {} after {:?}", attempt, delay);
            sleep(delay).await;
        }

        match send_tcp_once(addr, message, timeout).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or(Error::Timeout))
}

/// Single TCP send/receive attempt.
async fn send_tcp_once(addr: &str, message: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    // Connect with timeout.
    let stream = with_timeout(timeout, TcpStream::connect_host(addr))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(Error::Io)?;

    // Disable Nagle for lower latency.
    stream.set_nodelay(true).map_err(Error::Io)?;
    let (mut reader, mut writer) = stream.into_split();

    // Send: 4-byte big-endian length prefix + DER bytes.
    let len = message.len() as u32;
    let len_bytes = len.to_be_bytes();

    with_timeout(timeout, async {
        writer.write_all(&len_bytes).await.map_err(Error::Io)?;
        writer.write_all(message).await.map_err(Error::Io)?;
        writer.flush().await.map_err(Error::Io)?;
        trace!("kdc: TCP sent {} bytes", message.len());
        Ok::<(), Error>(())
    })
    .await
    .map_err(|_| Error::Timeout)??;

    // Receive: 4-byte big-endian length prefix.
    let mut len_buf = [0u8; 4];
    with_timeout(timeout, reader.read_exact(&mut len_buf))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Disconnected
            } else {
                Error::Io(e)
            }
        })?;

    let resp_len = u32::from_be_bytes(len_buf) as usize;
    if resp_len > MAX_KDC_FRAME_SIZE {
        return Err(Error::invalid_data(format!(
            "KDC TCP response length {} exceeds maximum {}",
            resp_len, MAX_KDC_FRAME_SIZE
        )));
    }

    // Read the response body.
    let mut buf = vec![0u8; resp_len];
    with_timeout(timeout, reader.read_exact(&mut buf))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                Error::Disconnected
            } else {
                Error::Io(e)
            }
        })?;

    trace!("kdc: TCP received {} bytes", resp_len);
    Ok(buf)
}

/// Detect KRB_ERR_RESPONSE_TOO_BIG (error code 52) in a KRB-ERROR response.
///
/// KRB-ERROR is APPLICATION [30] (tag 0x7e). We parse just enough DER
/// to extract the error-code field (context tag [6]) without a full
/// ASN.1 parser.
fn is_response_too_big(response: &[u8]) -> bool {
    // KRB-ERROR starts with APPLICATION [30] = 0x7e.
    if response.is_empty() || response[0] != 0x7e {
        return false;
    }

    match extract_krb_error_code(response) {
        Some(code) => code == KRB_ERR_RESPONSE_TOO_BIG,
        None => false,
    }
}

/// Extract the error-code from a KRB-ERROR message.
///
/// KRB-ERROR structure (simplified DER):
/// ```text
/// APPLICATION [30] {
///   SEQUENCE {
///     [0] pvno INTEGER,
///     [1] msg-type INTEGER,
///     [2] ctime (optional),
///     [3] cusec (optional),
///     [4] stime,
///     [5] susec,
///     [6] error-code INTEGER,   <-- we want this
///     ...
///   }
/// }
/// ```
fn extract_krb_error_code(data: &[u8]) -> Option<u32> {
    let mut pos = 0;

    // Skip APPLICATION [30] tag.
    if pos >= data.len() || data[pos] != 0x7e {
        return None;
    }
    pos += 1;
    pos = skip_der_length(data, pos)?;

    // Skip SEQUENCE tag (0x30).
    if pos >= data.len() || data[pos] != 0x30 {
        return None;
    }
    pos += 1;
    pos = skip_der_length(data, pos)?;

    // Now iterate through context-tagged fields until we find [6].
    loop {
        if pos >= data.len() {
            return None;
        }

        let tag = data[pos];
        // Context tags are 0xa0..0xbf for constructed.
        if tag & 0xe0 != 0xa0 {
            return None;
        }
        let tag_num = tag & 0x1f;
        pos += 1;

        let (field_len, new_pos) = read_der_length(data, pos)?;
        let field_end = new_pos + field_len;

        if tag_num == 6 {
            // This field contains an INTEGER with the error code.
            return parse_der_integer(data, new_pos);
        }

        pos = field_end;
    }
}

/// Skip a DER length field and return the position after it.
fn skip_der_length(data: &[u8], pos: usize) -> Option<usize> {
    let (_len, new_pos) = read_der_length(data, pos)?;
    Some(new_pos)
}

/// Read a DER length field, returning (length, position_after_length).
fn read_der_length(data: &[u8], pos: usize) -> Option<(usize, usize)> {
    if pos >= data.len() {
        return None;
    }

    let first = data[pos];
    match first.cmp(&0x80) {
        std::cmp::Ordering::Less => {
            // Short form: length is the byte itself.
            Some((first as usize, pos + 1))
        }
        std::cmp::Ordering::Equal => {
            // Indefinite length, not used in DER.
            None
        }
        std::cmp::Ordering::Greater => {
            // Long form: first byte & 0x7f = number of subsequent length bytes.
            let num_bytes = (first & 0x7f) as usize;
            if num_bytes > 4 || pos + 1 + num_bytes > data.len() {
                return None;
            }
            let mut length: usize = 0;
            for i in 0..num_bytes {
                length = (length << 8) | (data[pos + 1 + i] as usize);
            }
            Some((length, pos + 1 + num_bytes))
        }
    }
}

/// Parse a DER INTEGER at the given position, returning its value as u32.
fn parse_der_integer(data: &[u8], pos: usize) -> Option<u32> {
    if pos >= data.len() || data[pos] != 0x02 {
        return None;
    }
    let (len, val_pos) = read_der_length(data, pos + 1)?;
    if val_pos + len > data.len() || len == 0 || len > 4 {
        return None;
    }

    let mut value: u32 = 0;
    for i in 0..len {
        value = (value << 8) | (data[val_pos + i] as u32);
    }
    Some(value)
}

/// Discover KDC addresses for a realm via DNS SRV records.
///
/// Looks up `_kerberos._udp.{realm}` and `_kerberos._tcp.{realm}`.
/// Returns addresses sorted by priority.
///
/// For now, this is a placeholder -- initial implementation uses
/// the hardcoded address from KdcConfig. DNS SRV discovery will
/// be added in a future version.
pub async fn discover_kdc(_realm: &str) -> Vec<String> {
    // Placeholder: DNS SRV lookup not yet implemented.
    // Callers should use KdcConfig.address directly.
    debug!("kdc: DNS SRV discovery not yet implemented, returning empty list");
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    // The fake KDCs run on tokio's own sockets.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, UdpSocket};

    // ── DER parsing tests ──────────────────────────────────────────

    #[test]
    fn read_der_length_short_form() {
        assert_eq!(read_der_length(&[0x05], 0), Some((5, 1)));
        assert_eq!(read_der_length(&[0x7f], 0), Some((127, 1)));
        assert_eq!(read_der_length(&[0x00], 0), Some((0, 1)));
    }

    #[test]
    fn read_der_length_long_form_one_byte() {
        // 0x81, 0x80 = 128 bytes
        assert_eq!(read_der_length(&[0x81, 0x80], 0), Some((128, 2)));
    }

    #[test]
    fn read_der_length_long_form_two_bytes() {
        // 0x82, 0x01, 0x00 = 256 bytes
        assert_eq!(read_der_length(&[0x82, 0x01, 0x00], 0), Some((256, 3)));
    }

    #[test]
    fn read_der_length_indefinite_returns_none() {
        assert_eq!(read_der_length(&[0x80], 0), None);
    }

    #[test]
    fn read_der_length_truncated_returns_none() {
        // Says 2 length bytes follow but only 1 is present.
        assert_eq!(read_der_length(&[0x82, 0x01], 0), None);
    }

    #[test]
    fn parse_der_integer_single_byte() {
        // INTEGER tag 0x02, length 1, value 52.
        assert_eq!(parse_der_integer(&[0x02, 0x01, 0x34], 0), Some(52));
    }

    #[test]
    fn parse_der_integer_two_bytes() {
        // INTEGER tag 0x02, length 2, value 0x0100 = 256.
        assert_eq!(parse_der_integer(&[0x02, 0x02, 0x01, 0x00], 0), Some(256));
    }

    #[test]
    fn parse_der_integer_not_integer_tag() {
        assert_eq!(parse_der_integer(&[0x03, 0x01, 0x34], 0), None);
    }

    // ── KRB-ERROR detection tests ──────────────────────────────────

    /// Build a minimal KRB-ERROR with the given error code.
    ///
    /// This constructs a valid DER-encoded KRB-ERROR with fields:
    /// [0] pvno = 5, [1] msg-type = 30, [4] stime, [5] susec = 0,
    /// [6] error-code = the given code.
    fn build_krb_error(error_code: u32) -> Vec<u8> {
        // Helper: wrap value in context tag.
        fn context_tag(tag_num: u8, contents: &[u8]) -> Vec<u8> {
            let mut out = vec![0xa0 | tag_num];
            push_der_length(&mut out, contents.len());
            out.extend_from_slice(contents);
            out
        }

        // Helper: encode a DER INTEGER.
        fn der_integer(value: u32) -> Vec<u8> {
            // Encode as minimal bytes.
            let bytes = if value == 0 {
                vec![0x00]
            } else if value < 0x80 {
                vec![value as u8]
            } else if value < 0x8000 {
                vec![(value >> 8) as u8, (value & 0xff) as u8]
            } else if value < 0x800000 {
                vec![
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    (value & 0xff) as u8,
                ]
            } else {
                vec![
                    (value >> 24) as u8,
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    (value & 0xff) as u8,
                ]
            };
            let mut out = vec![0x02];
            push_der_length(&mut out, bytes.len());
            out.extend_from_slice(&bytes);
            out
        }

        fn push_der_length(out: &mut Vec<u8>, len: usize) {
            if len < 0x80 {
                out.push(len as u8);
            } else if len < 0x100 {
                out.push(0x81);
                out.push(len as u8);
            } else {
                out.push(0x82);
                out.push((len >> 8) as u8);
                out.push((len & 0xff) as u8);
            }
        }

        // Build the SEQUENCE contents.
        let pvno = context_tag(0, &der_integer(5));
        let msg_type = context_tag(1, &der_integer(30));
        // Skip [2] ctime and [3] cusec (optional).
        // [4] stime: GeneralizedTime "20250101000000Z"
        let stime_val = b"20250101000000Z";
        let mut stime_der = vec![0x18]; // GeneralizedTime tag
        push_der_length(&mut stime_der, stime_val.len());
        stime_der.extend_from_slice(stime_val);
        let stime = context_tag(4, &stime_der);
        let susec = context_tag(5, &der_integer(0));
        let error_code_field = context_tag(6, &der_integer(error_code));

        let mut seq_contents = Vec::new();
        seq_contents.extend_from_slice(&pvno);
        seq_contents.extend_from_slice(&msg_type);
        seq_contents.extend_from_slice(&stime);
        seq_contents.extend_from_slice(&susec);
        seq_contents.extend_from_slice(&error_code_field);

        // Wrap in SEQUENCE.
        let mut seq = vec![0x30];
        push_der_length(&mut seq, seq_contents.len());
        seq.extend_from_slice(&seq_contents);

        // Wrap in APPLICATION [30].
        let mut msg = vec![0x7e];
        push_der_length(&mut msg, seq.len());
        msg.extend_from_slice(&seq);

        msg
    }

    #[test]
    fn is_response_too_big_detects_error_52() {
        let error = build_krb_error(KRB_ERR_RESPONSE_TOO_BIG);
        assert!(is_response_too_big(&error));
    }

    #[test]
    fn is_response_too_big_ignores_other_errors() {
        // Error code 6 = KDC_ERR_C_PRINCIPAL_UNKNOWN
        let error = build_krb_error(6);
        assert!(!is_response_too_big(&error));
    }

    #[test]
    fn is_response_too_big_ignores_non_error_messages() {
        // AS-REP starts with APPLICATION [11] = 0x6b
        assert!(!is_response_too_big(&[0x6b, 0x03, 0x30, 0x01, 0x00]));
    }

    #[test]
    fn is_response_too_big_handles_empty_response() {
        assert!(!is_response_too_big(&[]));
    }

    #[test]
    fn is_response_too_big_handles_truncated_response() {
        // Just the APPLICATION tag and nothing else.
        assert!(!is_response_too_big(&[0x7e]));
        assert!(!is_response_too_big(&[0x7e, 0x00]));
    }

    #[test]
    fn extract_error_code_from_valid_krb_error() {
        let error = build_krb_error(25);
        assert_eq!(extract_krb_error_code(&error), Some(25));
    }

    #[test]
    fn extract_error_code_returns_none_for_non_error() {
        assert_eq!(
            extract_krb_error_code(&[0x6b, 0x03, 0x30, 0x01, 0x00]),
            None
        );
    }

    // ── Address resolution tests ───────────────────────────────────

    #[test]
    fn resolve_address_adds_default_port() {
        assert_eq!(resolve_address("kdc.example.com"), "kdc.example.com:88");
    }

    #[test]
    fn resolve_address_preserves_explicit_port() {
        assert_eq!(
            resolve_address("kdc.example.com:8888"),
            "kdc.example.com:8888"
        );
    }

    #[test]
    fn resolve_address_ip_no_port() {
        assert_eq!(resolve_address("10.0.0.1"), "10.0.0.1:88");
    }

    #[test]
    fn resolve_address_ip_with_port() {
        assert_eq!(resolve_address("10.0.0.1:88"), "10.0.0.1:88");
    }

    // ── UDP transport tests ────────────────────────────────────────

    #[tokio::test]
    async fn udp_send_receive() {
        // Set up a mock KDC that echoes the request back.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut buf = vec![0u8; UDP_MAX_SIZE];
            let (n, src) = server.recv_from(&mut buf).await.unwrap();
            // Echo back the message.
            server.send_to(&buf[..n], src).await.unwrap();
        });

        let message = b"test-kerberos-message";
        let result = send_udp(&server_addr.to_string(), message, Duration::from_secs(5)).await;

        assert!(
            result.is_ok(),
            "UDP send/receive failed: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), message);

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn udp_timeout_on_no_response() {
        // Bind a server socket but never read from it.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        // Use very short timeout and only 1 retry attempt to keep test fast.
        // We can't change MAX_RETRIES, but we use a very short timeout so
        // all 3 retries finish quickly.
        let result = send_udp(
            &server_addr.to_string(),
            b"hello",
            Duration::from_millis(50),
        )
        .await;

        assert!(result.is_err());
        assert!(
            matches!(result.as_ref().unwrap_err(), Error::Timeout),
            "expected Timeout, got: {:?}",
            result.unwrap_err()
        );

        drop(server);
    }

    // ── TCP transport tests ────────────────────────────────────────

    #[tokio::test]
    async fn tcp_send_receive() {
        // Set up a mock KDC that reads a length-prefixed message and
        // sends back a length-prefixed response.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read 4-byte length prefix.
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let msg_len = u32::from_be_bytes(len_buf) as usize;

            // Read the message body.
            let mut msg = vec![0u8; msg_len];
            stream.read_exact(&mut msg).await.unwrap();

            // Echo back with length prefix.
            let response = b"kdc-response";
            let resp_len = (response.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).await.unwrap();
            stream.write_all(response).await.unwrap();
            stream.flush().await.unwrap();
        });

        let result = send_tcp(&addr.to_string(), b"test-request", Duration::from_secs(5)).await;

        assert!(
            result.is_ok(),
            "TCP send/receive failed: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), b"kdc-response");

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_timeout_on_no_response() {
        // Set up a server that accepts but never responds.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            // Hold the connection open but never respond.
            tokio::time::sleep(Duration::from_secs(10)).await;
            drop(stream);
        });

        let result = send_tcp_once(&addr.to_string(), b"hello", Duration::from_millis(100)).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Timeout),
            "expected Timeout, got: {err}"
        );

        server_task.abort();
    }

    #[tokio::test]
    async fn tcp_truncated_response() {
        // Server sends a length prefix saying 100 bytes, then disconnects.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read the request (don't care about contents).
            let mut len_buf = [0u8; 4];
            let _ = stream.read_exact(&mut len_buf).await;
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut discard = vec![0u8; msg_len];
            let _ = stream.read_exact(&mut discard).await;

            // Send response with length 100 but only 5 bytes of data, then close.
            let resp_len = 100u32.to_be_bytes();
            stream.write_all(&resp_len).await.unwrap();
            stream
                .write_all(&[0x01, 0x02, 0x03, 0x04, 0x05])
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
        });

        let result = send_tcp_once(&addr.to_string(), b"hello", Duration::from_secs(5)).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::Disconnected),
            "expected Disconnected for truncated response, got: {err}"
        );

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_oversized_length_rejected() {
        // Server sends a length prefix larger than MAX_KDC_FRAME_SIZE.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read request.
            let mut len_buf = [0u8; 4];
            let _ = stream.read_exact(&mut len_buf).await;
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut discard = vec![0u8; msg_len];
            let _ = stream.read_exact(&mut discard).await;

            // Send absurdly large length.
            let resp_len = (MAX_KDC_FRAME_SIZE as u32 + 1).to_be_bytes();
            stream.write_all(&resp_len).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let result = send_tcp_once(&addr.to_string(), b"hello", Duration::from_secs(5)).await;

        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("exceeds maximum"),
            "expected 'exceeds maximum' error, got: {err_str}"
        );

        server_task.abort();
    }

    // ── send_to_kdc tests ──────────────────────────────────────────

    #[tokio::test]
    async fn send_to_kdc_udp_success() {
        // Set up a UDP mock KDC.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut buf = vec![0u8; UDP_MAX_SIZE];
            let (n, src) = server.recv_from(&mut buf).await.unwrap();
            // Respond with a fake AS-REP (not a KRB-ERROR).
            let response = b"\x6b\x05\x30\x03\x02\x01\x05"; // Fake AS-REP-like
            server.send_to(response, src).await.unwrap();
            drop(buf[..n].to_vec()); // acknowledge we received
        });

        let config = KdcConfig {
            address: server_addr.to_string(),
            timeout: Duration::from_secs(5),
        };

        let result = send_to_kdc(&config, b"as-req").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"\x6b\x05\x30\x03\x02\x01\x05");

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn send_to_kdc_udp_too_big_falls_back_to_tcp() {
        // Set up a UDP server that returns KRB_ERR_RESPONSE_TOO_BIG
        // and a TCP server that returns a real response. The fallback
        // path uses one `KdcConfig.address`, so both servers must share
        // a port.
        //
        // Bind TCP first (more restrictive) and then UDP to its port.
        // On Windows Server, the OS port allocator can hand out an
        // ephemeral port that's in an excluded range for the other
        // protocol (WSAEACCES / 10013). Retry a few times if so;
        // a fresh `:0` lottery picks a different port each attempt.
        let (udp_server, tcp_listener) = {
            let mut last_err: Option<std::io::Error> = None;
            let mut bound = None;
            for _ in 0..10 {
                let tcp = match TcpListener::bind("127.0.0.1:0").await {
                    Ok(l) => l,
                    Err(e) => {
                        last_err = Some(e);
                        continue;
                    }
                };
                let port = tcp.local_addr().unwrap().port();
                match UdpSocket::bind(format!("127.0.0.1:{port}")).await {
                    Ok(udp) => {
                        bound = Some((udp, tcp));
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        // TCP listener drops here; try a new port.
                    }
                }
            }
            bound.unwrap_or_else(|| {
                panic!("could not co-bind UDP+TCP on a shared loopback port in 10 attempts: {last_err:?}")
            })
        };
        let udp_addr = udp_server.local_addr().unwrap();

        let udp_task = tokio::spawn(async move {
            let mut buf = vec![0u8; UDP_MAX_SIZE];
            let (_, src) = udp_server.recv_from(&mut buf).await.unwrap();
            let error = build_krb_error(KRB_ERR_RESPONSE_TOO_BIG);
            udp_server.send_to(&error, src).await.unwrap();
        });

        let tcp_task = tokio::spawn(async move {
            let (mut stream, _) = tcp_listener.accept().await.unwrap();
            // Read request.
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let msg_len = u32::from_be_bytes(len_buf) as usize;
            let mut msg = vec![0u8; msg_len];
            stream.read_exact(&mut msg).await.unwrap();

            // Send TCP response.
            let response = b"tcp-kdc-response";
            let resp_len = (response.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).await.unwrap();
            stream.write_all(response).await.unwrap();
            stream.flush().await.unwrap();
        });

        let config = KdcConfig {
            address: udp_addr.to_string(),
            timeout: Duration::from_secs(5),
        };

        let result = send_to_kdc(&config, b"as-req-large").await;
        assert!(result.is_ok(), "send_to_kdc failed: {:?}", result.err());
        assert_eq!(result.unwrap(), b"tcp-kdc-response");

        udp_task.await.unwrap();
        tcp_task.await.unwrap();
    }

    // ── discover_kdc tests ─────────────────────────────────────────

    #[tokio::test]
    async fn discover_kdc_returns_empty_placeholder() {
        let result = discover_kdc("EXAMPLE.COM").await;
        assert!(result.is_empty());
    }
}
