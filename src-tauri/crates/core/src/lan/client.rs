//! The joining side.
//!
//! The caller checks offline mode before calling in: a connection out of the
//! machine is an outbound path like any other
//! ([invariant](../../../../../docs/architecture/invariants.md#process-wide-offline-mode)).

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{
    verify_tls12_signature, verify_tls13_signature, CryptoProvider,
};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{self, ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;

use super::address::PeerFilter;
use super::cert::{Fingerprint, SERVER_NAME};
use super::code::Code;
use super::exchange::GuestExchange;
use super::handshake::{GuestHandshake, HandshakeRefusal, HostHandshake};
use super::listener::{export, next_text, send};
use super::{provider, websocket_config, HANDSHAKE_TIMEOUT, HANDSHAKE_VERSION};

/// TLS over a TCP connection, as a guest holds it.
pub type ClientStream = tokio_rustls::client::TlsStream<TcpStream>;

/// A connection to a listener.
pub struct Joined {
    pub socket: WebSocketStream<ClientStream>,
    /// The listener's certificate: what to pin when coming back.
    pub fingerprint: Fingerprint,
}

/// Why joining failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
    /// The address is not one the filter lets this machine connect to: an
    /// mDNS answer or a link can name anything.
    #[error("not a private address")]
    NotPrivate,
    #[error("the listener cannot be reached")]
    Unreachable,
    #[error("the TLS handshake failed")]
    Tls,
    /// The listener's certificate is not the one pinned.
    #[error("the listener's certificate is not the expected one")]
    FingerprintMismatch,
    /// The host's confirmation did not match: a wrong code, or not the host
    /// the code was given by. Nothing was sent that it could use.
    #[error("the code was not accepted")]
    CodeRejected,
    #[error("refused: {0}")]
    Refused(HandshakeRefusal, Option<u64>),
    #[error("the listener answered something unexpected")]
    Protocol,
    #[error("the connection closed")]
    Closed,
    #[error("the listener took too long")]
    TimedOut,
}

/// Join the listener at `addr` with `code`. `pin`, when known (an invitation
/// link), is checked before anything else is sent; without it, the exchange
/// is what proves the listener is the host.
pub async fn join(
    addr: SocketAddr,
    pin: Option<Fingerprint>,
    code: &Code,
    context: &str,
    filter: PeerFilter,
) -> Result<Joined, JoinError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let (mut joined, exporter) = connect(addr, pin, filter).await?;
        let (exchange, message) = GuestExchange::start(code, context);
        let start = GuestHandshake::ExchangeStart {
            version: HANDSHAKE_VERSION,
            context: context.to_string(),
            guest: hex::encode(message),
        };
        if !send(&mut joined.socket, &start).await {
            return Err(JoinError::Closed);
        }
        let (host, host_confirm) = match read_host(&mut joined.socket).await? {
            HostHandshake::ExchangeReply { host, confirm } => (host, confirm),
            HostHandshake::ExchangeRefused {
                reason,
                retry_after_ms,
            } => return Err(JoinError::Refused(reason, retry_after_ms)),
            HostHandshake::ExchangeDone => return Err(JoinError::Protocol),
        };
        let host = hex::decode(host).map_err(|_| JoinError::Protocol)?;
        let host_confirm = hex::decode(host_confirm).map_err(|_| JoinError::Protocol)?;
        // A mismatch stops here, before the guest's own confirmation leaves.
        let confirm = exchange
            .finish(&host, &host_confirm, &exporter)
            .map_err(|_| JoinError::CodeRejected)?;
        let confirm = GuestHandshake::ExchangeConfirm {
            confirm: hex::encode(confirm),
        };
        if !send(&mut joined.socket, &confirm).await {
            return Err(JoinError::Closed);
        }
        match read_host(&mut joined.socket).await? {
            HostHandshake::ExchangeDone => Ok(joined),
            HostHandshake::ExchangeRefused {
                reason,
                retry_after_ms,
            } => Err(JoinError::Refused(reason, retry_after_ms)),
            HostHandshake::ExchangeReply { .. } => Err(JoinError::Protocol),
        }
    })
    .await
    .map_err(|_| JoinError::TimedOut)?
}

/// Connect again to a listener already joined, whose certificate is `pin`,
/// without the exchange: the caller's first message carries what it was
/// given (a token).
pub async fn reconnect(
    addr: SocketAddr,
    pin: Fingerprint,
    filter: PeerFilter,
) -> Result<Joined, JoinError> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, connect(addr, Some(pin), filter))
        .await
        .map_err(|_| JoinError::TimedOut)?
        .map(|(joined, _)| joined)
}

/// TLS and the WebSocket, without the exchange: the connection and its TLS
/// exporter.
pub(super) async fn connect(
    addr: SocketAddr,
    pin: Option<Fingerprint>,
    filter: PeerFilter,
) -> Result<(Joined, Vec<u8>), JoinError> {
    if !filter.admits(addr.ip()) {
        return Err(JoinError::NotPrivate);
    }
    let provider = provider();
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| JoinError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
        .with_no_client_auth();
    config.enable_sni = false;

    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|_| JoinError::Unreachable)?;
    let name = ServerName::try_from(SERVER_NAME).map_err(|_| JoinError::Tls)?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| {
            let pinned_wrong = e
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                .is_some_and(|e| matches!(e, rustls::Error::InvalidCertificate(_)));
            if pinned_wrong {
                JoinError::FingerprintMismatch
            } else {
                JoinError::Tls
            }
        })?;
    let (_, conn) = tls.get_ref();
    let fingerprint = conn
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|cert| Fingerprint::of(cert))
        .ok_or(JoinError::Tls)?;
    let exporter = export(conn).ok_or(JoinError::Tls)?;
    let (socket, _) = tokio_tungstenite::client_async_with_config(
        format!("ws://{SERVER_NAME}/"),
        tls,
        Some(websocket_config()),
    )
    .await
    .map_err(|_| JoinError::Protocol)?;
    Ok((
        Joined {
            socket,
            fingerprint,
        },
        exporter,
    ))
}

async fn read_host(socket: &mut WebSocketStream<ClientStream>) -> Result<HostHandshake, JoinError> {
    let text = next_text(socket).await.ok_or(JoinError::Closed)?;
    serde_json::from_str(&text).map_err(|_| JoinError::Protocol)
}

/// Accepts the listener's certificate when it is the pinned one, or any
/// certificate when nothing is pinned (the exchange then proves who the
/// listener is). The handshake's signatures are still checked, so the
/// listener holds the key of the certificate it shows.
#[derive(Debug)]
struct Pinned {
    pin: Option<Fingerprint>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.pin {
            Some(pin) if Fingerprint::of(end_entity) != pin => {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
