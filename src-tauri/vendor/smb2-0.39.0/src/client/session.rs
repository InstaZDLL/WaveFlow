//! Authenticated SMB2 session.
//!
//! The [`Session`] type manages the multi-round-trip SESSION_SETUP exchange
//! (NTLM authentication), key derivation, and signing activation.

use log::{debug, info, trace, warn};

use crate::auth::ntlm::{NtlmAuthenticator, NtlmCredentials};
use crate::client::connection::Connection;
use crate::crypto::kdf::derive_session_keys;
use crate::crypto::signing::{self, algorithm_for_dialect, SigningAlgorithm};
use crate::error::{Result, SignatureFailure};
use crate::msg::header::Header;
use crate::msg::session_setup::{SessionFlags, SessionSetupRequest, SessionSetupResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::types::flags::{Capabilities, SecurityMode};
use crate::types::status::NtStatus;
use crate::types::{Command, Dialect, SessionId};
use crate::Error;

use crate::msg::session_setup::SessionSetupRequestFlags;

/// An authenticated SMB2 session with derived keys.
///
/// `Debug` shows the keys as `***`.
pub struct Session {
    /// The session ID assigned by the server.
    pub session_id: SessionId,
    /// Key used to sign outgoing messages.
    pub signing_key: Vec<u8>,
    /// Key used to encrypt outgoing messages (SMB 3.x).
    pub encryption_key: Option<Vec<u8>>,
    /// Key used to decrypt incoming messages (SMB 3.x).
    pub decryption_key: Option<Vec<u8>>,
    /// The signing algorithm to use.
    pub signing_algorithm: SigningAlgorithm,
    /// Whether outgoing messages should be signed.
    pub should_sign: bool,
    /// Whether outgoing messages should be encrypted.
    pub should_encrypt: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::crypto::Redacted;
        // Destructured, so a new field can't be left out of the output.
        let Self {
            session_id,
            signing_key: _,
            encryption_key,
            decryption_key,
            signing_algorithm,
            should_sign,
            should_encrypt,
        } = self;
        f.debug_struct("Session")
            .field("session_id", session_id)
            .field("signing_key", &Redacted)
            .field("encryption_key", &encryption_key.as_ref().map(|_| Redacted))
            .field("decryption_key", &decryption_key.as_ref().map(|_| Redacted))
            .field("signing_algorithm", signing_algorithm)
            .field("should_sign", should_sign)
            .field("should_encrypt", should_encrypt)
            .finish()
    }
}

impl Session {
    /// A by-value copy.
    ///
    /// `Session` deliberately isn't `Clone`: it holds signing and encryption
    /// keys, and a derive would make duplicating them invisible. Copying here
    /// is explicit and has one caller — [`Connection::adopt_session`], which
    /// keeps the current session reachable from a `Connection` alone.
    pub(crate) fn snapshot(&self) -> Session {
        Session {
            session_id: self.session_id,
            signing_key: self.signing_key.clone(),
            encryption_key: self.encryption_key.clone(),
            decryption_key: self.decryption_key.clone(),
            signing_algorithm: self.signing_algorithm,
            should_sign: self.should_sign,
            should_encrypt: self.should_encrypt,
        }
    }

    /// Perform the multi-round-trip SESSION_SETUP exchange.
    ///
    /// Steps:
    /// 1. Send NTLM NEGOTIATE_MESSAGE in SESSION_SETUP.
    /// 2. Receive STATUS_MORE_PROCESSING_REQUIRED with CHALLENGE_MESSAGE.
    /// 3. Update preauth hash with request+response.
    /// 4. Send NTLM AUTHENTICATE_MESSAGE in SESSION_SETUP.
    /// 5. Receive STATUS_SUCCESS with session flags.
    /// 6. Update preauth hash with request+response.
    /// 7. Derive signing/encryption keys.
    /// 8. Prove the final response (signature, no guest stand-in for a named
    ///    account) before trusting the session flags it carries.
    /// 9. Activate signing on the connection.
    ///
    /// # Errors
    ///
    /// Besides the server's own refusal (`STATUS_LOGON_FAILURE` and kin, as
    /// [`Error::Protocol`]), a login with a username other than `Guest` fails
    /// with [`Error::GuestSessionRefused`] when the server answers it with a
    /// guest or anonymous session, and with [`Error::SignatureInvalid`] when
    /// the final response is missing the signature it must carry or doesn't
    /// verify.
    pub async fn setup(
        conn: &mut Connection,
        username: &str,
        password: &str,
        domain: &str,
    ) -> Result<Session> {
        let params = conn
            .params()
            .ok_or_else(|| Error::invalid_data("negotiate must complete before session setup"))?
            .clone();

        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: username.to_string(),
            password: password.to_string(),
            domain: domain.to_string(),
        });

        // Clone the preauth hasher for this session (spec: per-session hash).
        let mut session_hasher = conn.preauth_hasher().clone();

        // ── Round 1: NEGOTIATE_MESSAGE ──
        debug!("session: round 1, sending NTLM negotiate");

        let type1_bytes = auth.negotiate();

        let req1 = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: conn.previous_session_id().0,
            security_buffer: type1_bytes,
        };

        let (frame1, req1_raw) = conn
            .execute_capturing_request(Command::SessionSetup, &req1, None)
            .await?;

        // Update session preauth hash with request.
        session_hasher.update(&req1_raw);

        let resp1_header = frame1.header;
        let resp1_body = frame1.body;

        // Update session preauth hash with response.
        session_hasher.update(&frame1.raw);

        if resp1_header.command != Command::SessionSetup {
            return Err(Error::invalid_data(format!(
                "expected SessionSetup response, got {:?}",
                resp1_header.command
            )));
        }

        if !resp1_header.status.is_more_processing_required() {
            if resp1_header.status.is_error() {
                return Err(Error::Protocol {
                    status: resp1_header.status,
                    command: Command::SessionSetup,
                });
            }
            return Err(Error::invalid_data(
                "expected STATUS_MORE_PROCESSING_REQUIRED, got success on first round",
            ));
        }

        // The server assigned a session ID -- use it for subsequent requests.
        debug!(
            "session: round 1 complete, status={:?}, session_id={}",
            resp1_header.status, resp1_header.session_id
        );
        conn.set_session_id(resp1_header.session_id);

        // Parse the challenge response.
        let mut cursor1 = ReadCursor::new(&resp1_body);
        let setup_resp1 = SessionSetupResponse::unpack(&mut cursor1)?;

        // ── Round 2: AUTHENTICATE_MESSAGE ──
        debug!("session: round 2, sending NTLM authenticate");

        let type3_bytes = auth.authenticate(&setup_resp1.security_buffer)?;

        let req2 = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: conn.previous_session_id().0,
            security_buffer: type3_bytes,
        };

        let (frame2, req2_raw) = conn
            .execute_capturing_request(Command::SessionSetup, &req2, None)
            .await?;

        // Update session preauth hash with the request ONLY.
        // The final SESSION_SETUP response (STATUS_SUCCESS) is NOT
        // included in the preauth hash (spec section 3.2.5.3.1).
        // Only STATUS_MORE_PROCESSING_REQUIRED responses are hashed.
        session_hasher.update(&req2_raw);

        let resp2_header = frame2.header;
        let resp2_body = frame2.body;

        // Do NOT hash the success response -- the preauth hash used for
        // key derivation contains only messages up to (and including)
        // the final authenticate request, not the success response.

        if resp2_header.command != Command::SessionSetup {
            return Err(Error::invalid_data(format!(
                "expected SessionSetup response, got {:?}",
                resp2_header.command
            )));
        }

        if resp2_header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: resp2_header.status,
                command: Command::SessionSetup,
            });
        }

        // Parse the final response.
        let mut cursor2 = ReadCursor::new(&resp2_body);
        let setup_resp2 = SessionSetupResponse::unpack(&mut cursor2)?;

        let session_id = resp2_header.session_id;
        conn.set_session_id(session_id);

        // Get the session key from NTLM.
        let session_key = auth
            .session_key()
            .ok_or_else(|| Error::Auth {
                message: "NTLM did not produce a session key".to_string(),
            })?
            .to_vec();

        // Determine signing algorithm.
        let gmac_negotiated = params.gmac_negotiated;
        let signing_algorithm = algorithm_for_dialect(params.dialect, gmac_negotiated);
        debug!(
            "session: signing_algo={:?}, dialect={}",
            signing_algorithm, params.dialect
        );

        // Derive keys for SMB 3.x, or use session key directly for SMB 2.x.
        trace!(
            "session: deriving keys, session_key_len={}",
            session_key.len()
        );
        let (signing_key, encryption_key, decryption_key) = match params.dialect {
            Dialect::Smb3_0 | Dialect::Smb3_0_2 => {
                let keys = derive_session_keys(&session_key, params.dialect, None, 128);
                (
                    keys.signing_key,
                    Some(keys.encryption_key),
                    Some(keys.decryption_key),
                )
            }
            Dialect::Smb3_1_1 => {
                // Key length: 256 bits only for AES-256 ciphers. GMAC signing
                // uses AES-128-GCM internally, so it needs 128-bit (16-byte) keys.
                let key_len_bits = match params.cipher {
                    Some(crate::crypto::encryption::Cipher::Aes256Ccm)
                    | Some(crate::crypto::encryption::Cipher::Aes256Gcm) => 256,
                    _ => 128,
                };
                let keys = derive_session_keys(
                    &session_key,
                    Dialect::Smb3_1_1,
                    Some(session_hasher.value()),
                    key_len_bits,
                );
                (
                    keys.signing_key,
                    Some(keys.encryption_key),
                    Some(keys.decryption_key),
                )
            }
            _ => {
                // SMB 2.x: use session key directly for signing.
                (session_key.clone(), None, None)
            }
        };

        // Nothing below may read `session_flags` before this: they decide
        // whether signing and encryption turn on at all.
        let account = (!username.is_empty()).then_some(username);
        authenticate_final_response(
            &frame2.raw,
            &resp2_header,
            setup_resp2.session_flags,
            params.dialect,
            &signing_key,
            signing_algorithm,
            account,
        )?;

        // Determine if we should sign.
        let should_sign = params.signing_required
            || !setup_resp2.session_flags.is_guest() && !setup_resp2.session_flags.is_null();

        let should_encrypt = setup_resp2.session_flags.encrypt_data();

        // Activate signing on the connection.
        if should_sign {
            conn.activate_signing(signing_key.clone(), signing_algorithm);
        }

        // Activate encryption on the connection if the session requires it.
        // The cipher comes from negotiate contexts (SMB 3.1.1). If the server
        // didn't send one (for example, Samba with `smb encrypt = required` sometimes
        // omits the encryption context), fall back to AES-128-CCM which is
        // universally supported by all SMB 3.x servers.
        if should_encrypt {
            let cipher = params
                .cipher
                .unwrap_or(crate::crypto::encryption::Cipher::Aes128Ccm);
            if let (Some(ref enc_key), Some(ref dec_key)) = (&encryption_key, &decryption_key) {
                conn.activate_encryption(enc_key.clone(), dec_key.clone(), cipher);
            } else {
                warn!(
                    "session: encryption requested but missing keys, \
                     enc_key={}, dec_key={}",
                    encryption_key.is_some(),
                    decryption_key.is_some(),
                );
            }
        }

        info!(
            "session: established, session_id={}, sign={}, encrypt={}",
            session_id, should_sign, should_encrypt
        );

        let session = Session {
            session_id,
            signing_key,
            encryption_key,
            decryption_key,
            signing_algorithm,
            should_sign,
            should_encrypt,
        };
        // Publish it on the connection so a caller holding only a `Connection`
        // (an auto-reconnect that re-authenticated behind everyone's back, for
        // instance) can pick up the new keys instead of using a dead session's.
        conn.adopt_session(&session);
        Ok(session)
    }

    /// Perform Kerberos-based SESSION_SETUP.
    ///
    /// Authenticates against the KDC first (AS + TGS), then sends the
    /// SPNEGO-wrapped AP-REQ in SESSION_SETUP, one round. The AP-REP that
    /// completes mutual authentication rides in that STATUS_SUCCESS response.
    /// A server asking for another round (STATUS_MORE_PROCESSING_REQUIRED) is
    /// not followed: on SMB 3.x that fails the login, because that response is
    /// unsigned and the session flags it carries can't be trusted.
    ///
    /// The session key comes from the Kerberos TGS exchange, not from the
    /// SMB server response.
    /// Perform Kerberos-based SESSION_SETUP using a credential cache.
    ///
    /// Reads cached tickets from the ccache. If a service ticket for
    /// `cifs/<server_hostname>` is cached, uses it directly (no KDC needed).
    /// If only a TGT is cached, does a TGS exchange for the service ticket.
    pub async fn setup_kerberos_from_ccache(
        conn: &mut Connection,
        credentials: &crate::auth::kerberos::KerberosCredentials,
        server_hostname: &str,
        ccache: &crate::auth::kerberos::ccache::CCache,
    ) -> Result<Session> {
        let mut auth = crate::auth::kerberos::KerberosAuthenticator::new(credentials.clone());
        auth.authenticate_from_ccache(ccache, server_hostname)
            .await?;
        Self::setup_kerberos_with_auth(conn, &mut auth, &credentials.username).await
    }

    /// Perform Kerberos-based SESSION_SETUP.
    ///
    /// Authenticates against the KDC first (AS + TGS), then sends the
    /// SPNEGO-wrapped AP-REQ in SESSION_SETUP, one round. The AP-REP that
    /// completes mutual authentication rides in that STATUS_SUCCESS response.
    /// A server asking for another round (STATUS_MORE_PROCESSING_REQUIRED) is
    /// not followed: on SMB 3.x that fails the login with
    /// [`Error::SignatureInvalid`], because that response is unsigned and the
    /// session flags it carries can't be trusted. The final response is
    /// proven the same way [`setup`](Self::setup) proves it, with the same
    /// errors.
    ///
    /// The session key comes from the Kerberos TGS exchange, not from the
    /// SMB server response.
    pub async fn setup_kerberos(
        conn: &mut Connection,
        credentials: &crate::auth::kerberos::KerberosCredentials,
        server_hostname: &str,
    ) -> Result<Session> {
        let mut auth = crate::auth::kerberos::KerberosAuthenticator::new(credentials.clone());
        auth.authenticate(server_hostname).await?;
        Self::setup_kerberos_with_auth(conn, &mut auth, &credentials.username).await
    }

    /// Shared Kerberos SESSION_SETUP logic used by both password-based
    /// and ccache-based authentication paths.
    async fn setup_kerberos_with_auth(
        conn: &mut Connection,
        auth: &mut crate::auth::kerberos::KerberosAuthenticator,
        account: &str,
    ) -> Result<Session> {
        let params = conn
            .params()
            .ok_or_else(|| Error::invalid_data("negotiate must complete before session setup"))?
            .clone();

        let token = auth
            .token()
            .ok_or_else(|| Error::Auth {
                message: "Kerberos authentication produced no token".to_string(),
            })?
            .to_vec();

        debug!("session: Kerberos auth complete, token_len={}", token.len());

        // Clone the preauth hasher for this session.
        let mut session_hasher = conn.preauth_hasher().clone();

        // Step 2: Send SPNEGO-wrapped AP-REQ in SESSION_SETUP.
        let req = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: conn.previous_session_id().0,
            security_buffer: token,
        };

        let (frame, req_raw) = conn
            .execute_capturing_request(Command::SessionSetup, &req, None)
            .await?;

        // Hash the request (same as NTLM round 1).
        session_hasher.update(&req_raw);

        let resp_header = frame.header;
        let resp_body = frame.body;
        let resp_raw = frame.raw;

        if resp_header.command != Command::SessionSetup {
            return Err(Error::invalid_data(format!(
                "expected SessionSetup response, got {:?}",
                resp_header.command
            )));
        }

        if resp_header.status != NtStatus::SUCCESS
            && !resp_header.status.is_more_processing_required()
        {
            return Err(Error::Protocol {
                status: resp_header.status,
                command: Command::SessionSetup,
            });
        }

        // The server assigned a session ID.
        let session_id = resp_header.session_id;
        conn.set_session_id(session_id);

        let mut cursor = ReadCursor::new(&resp_body);
        let setup_resp = SessionSetupResponse::unpack(&mut cursor)?;

        if resp_header.status.is_more_processing_required() {
            debug!(
                "session: Kerberos got MORE_PROCESSING_REQUIRED, session_id={}",
                session_id
            );

            // Hash the response per MS-SMB2 3.2.5.3.1.
            session_hasher.update(&resp_raw);
        }

        // Process the SPNEGO response token (AP-REP or KRB-ERROR).
        // This applies to both STATUS_SUCCESS and MORE_PROCESSING_REQUIRED —
        // the server may include an AP-REP with a sub-session key in either.
        if !setup_resp.security_buffer.is_empty() {
            let spnego_resp =
                crate::auth::spnego::parse_neg_token_resp(&setup_resp.security_buffer)?;
            debug!(
                "session: SPNEGO state={:?}, has_token={}, supported_mech={:02x?}",
                spnego_resp.neg_state,
                spnego_resp.response_token.is_some(),
                spnego_resp.supported_mech.as_deref().unwrap_or(&[]),
            );

            if let Some(ref token_bytes) = spnego_resp.response_token {
                auth.process_mutual_auth_token(token_bytes)?;
            }
        }

        // Get the session key AFTER processing the AP-REP (the server's
        // subkey may have overridden ours).
        //
        // Per MS-SMB2 3.2.5.3: "Session.SessionKey MUST be set to the first
        // 16 bytes of the cryptographic key queried from the GSS protocol."
        let full_key = auth.session_key().ok_or_else(|| Error::Auth {
            message: "Kerberos authentication produced no session key".to_string(),
        })?;
        let session_key = if full_key.len() > 16 {
            full_key[..16].to_vec()
        } else {
            full_key.to_vec()
        };

        debug!(
            "session: Kerberos session_key_len={} (truncated from {})",
            session_key.len(),
            full_key.len()
        );

        // Determine signing algorithm.
        let signing_algorithm = algorithm_for_dialect(params.dialect, params.gmac_negotiated);
        debug!(
            "session: Kerberos signing_algo={:?}, dialect={}",
            signing_algorithm, params.dialect
        );

        // Derive keys for SMB 3.x using the Kerberos session key.
        let (signing_key, encryption_key, decryption_key) = match params.dialect {
            Dialect::Smb3_0 | Dialect::Smb3_0_2 => {
                let keys = derive_session_keys(&session_key, params.dialect, None, 128);
                (
                    keys.signing_key,
                    Some(keys.encryption_key),
                    Some(keys.decryption_key),
                )
            }
            Dialect::Smb3_1_1 => {
                let key_len_bits = match params.cipher {
                    Some(crate::crypto::encryption::Cipher::Aes256Ccm)
                    | Some(crate::crypto::encryption::Cipher::Aes256Gcm) => 256,
                    _ => 128,
                };
                let keys = derive_session_keys(
                    &session_key,
                    Dialect::Smb3_1_1,
                    Some(session_hasher.value()),
                    key_len_bits,
                );
                (
                    keys.signing_key,
                    Some(keys.encryption_key),
                    Some(keys.decryption_key),
                )
            }
            _ => (session_key.clone(), None, None),
        };

        // A Kerberos login always names an account, so the final response
        // has to prove itself before its flags are believed.
        authenticate_final_response(
            &resp_raw,
            &resp_header,
            setup_resp.session_flags,
            params.dialect,
            &signing_key,
            signing_algorithm,
            Some(account),
        )?;

        let should_sign = params.signing_required
            || !setup_resp.session_flags.is_guest() && !setup_resp.session_flags.is_null();

        let should_encrypt = setup_resp.session_flags.encrypt_data();

        if should_sign {
            conn.activate_signing(signing_key.clone(), signing_algorithm);
        }

        if should_encrypt {
            let cipher = params
                .cipher
                .unwrap_or(crate::crypto::encryption::Cipher::Aes128Ccm);
            if let (Some(ref enc_key), Some(ref dec_key)) = (&encryption_key, &decryption_key) {
                conn.activate_encryption(enc_key.clone(), dec_key.clone(), cipher);
            }
        }

        info!(
            "session: Kerberos established, session_id={}, sign={}, encrypt={}",
            session_id, should_sign, should_encrypt
        );

        let session = Session {
            session_id,
            signing_key,
            encryption_key,
            decryption_key,
            signing_algorithm,
            should_sign,
            should_encrypt,
        };
        // Publish it on the connection so a caller holding only a `Connection`
        // (an auto-reconnect that re-authenticated behind everyone's back, for
        // instance) can pick up the new keys instead of using a dead session's.
        conn.adopt_session(&session);
        Ok(session)
    }
}

/// Whether a login named `username` asks for a guest session: the name
/// `Guest`, in any ASCII case, exactly. Consumers have long logged in to guest
/// shares this way, and some servers (Windows with anonymous access
/// restricted, some NAS configurations) accept the named `Guest` login while
/// refusing an anonymous one, so it must keep working. ❌ Don't widen it to
/// look-alikes (`DOMAIN\Guest`, `guests`): those are accounts, and a guest
/// answer to one is the downgrade this check exists for.
fn is_guest_on_purpose(username: &str) -> bool {
    username.eq_ignore_ascii_case("guest")
}

/// Decide whether the final SESSION_SETUP response can be believed, before
/// anything reads its `SessionFlags`.
///
/// Those flags decide whether the session is signed (`IS_GUEST`, `IS_NULL`)
/// and encrypted (`ENCRYPT_DATA`) at all, and they arrive in the one response
/// no session key protects yet. So an on-path attacker who sets `IS_GUEST` or
/// clears `ENCRYPT_DATA` there switches protection off for the whole session,
/// and every later check (pitfall 29) is checking nothing.
///
/// `account` is `None` when the caller asked for a guest or anonymous login;
/// then there is no secret key to prove anything with, and the server's
/// answer stands as it always has. With an account:
///
/// - **An account named `Guest` (any ASCII case) takes a guest or anonymous
///   session as asked for** ([`is_guest_on_purpose`]): that's what the name
///   requests, so granting it downgrades nothing. Answered with a full session
///   instead (an enabled Windows `Guest` account), it's verified like any
///   account below.
/// - **Any other account refuses a guest or anonymous session.** The caller
///   asked to be someone; a server that grants a guest session instead has either been
///   tampered with or (Samba's `map to guest = bad user`) is answering a wrong
///   password. MS-SMB2 § 3.2.5.3.1 makes the same call for a client that
///   requires signing. Checked first because a genuine guest response is never
///   signed, so the verification below would only produce a vaguer error.
/// - **On SMB 3.x the response must be signed, and verify.** A server MUST sign
///   it for a non-guest session (§ 3.3.5.5.3), and a client on 3.1.1 MUST
///   refuse one without `SMB2_FLAGS_SIGNED` (§ 3.2.5.3.1). The key comes from
///   the session key the attacker doesn't have, and on 3.1.1 from the preauth
///   hash too, so a good signature also vouches for the whole negotiation.
///   ❌ The flag can only fail this early, never let a response skip
///   verification: it travels in the same untrusted bytes.
/// - **On SMB 2.x it's verified if the server flagged it signed.** The spec
///   asks no more of a 2.x client, and with guest and anonymous already
///   refused there is nothing left in the response to downgrade: 2.x has no
///   encryption, and a non-guest session is always signed.
fn authenticate_final_response(
    raw: &[u8],
    header: &Header,
    flags: SessionFlags,
    dialect: Dialect,
    signing_key: &[u8],
    algorithm: SigningAlgorithm,
    account: Option<&str>,
) -> Result<()> {
    let Some(account) = account else {
        return Ok(());
    };

    if flags.is_guest() || flags.is_null() {
        if is_guest_on_purpose(account) {
            return Ok(());
        }
        let anonymous = !flags.is_guest();
        debug!(
            "session: server offered a {} session to a named login, refusing user={account:?}",
            if anonymous { "anonymous" } else { "guest" }
        );
        return Err(Error::GuestSessionRefused {
            username: account.to_string(),
            anonymous,
        });
    }

    let is_smb3 = matches!(
        dialect,
        Dialect::Smb3_0 | Dialect::Smb3_0_2 | Dialect::Smb3_1_1
    );
    let flagged_signed = header.flags.is_signed();
    if !flagged_signed && !is_smb3 {
        return Ok(());
    }
    if !flagged_signed {
        debug!(
            "session: final SESSION_SETUP response ({:?}) isn't flagged signed, refusing",
            header.status
        );
        return Err(Error::SignatureInvalid {
            command: Command::SessionSetup,
            message_id: header.message_id,
            reason: SignatureFailure::Missing,
        });
    }
    signing::verify_signature(raw, signing_key, algorithm, header.message_id.0, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::connection::{pack_message, Connection, NegotiatedParams};
    use crate::error::SignatureFailure;
    use crate::msg::header::Header;
    use crate::msg::session_setup::{SessionFlags, SessionSetupResponse};
    use crate::pack::Guid;
    use crate::transport::MockTransport;
    use crate::types::flags::Capabilities;
    use crate::types::status::NtStatus;
    use crate::types::{Command, Dialect, SessionId};
    use std::sync::Arc;

    /// Build a session setup response with the given status and session ID.
    fn build_session_setup_response(
        status: NtStatus,
        session_id: SessionId,
        security_buffer: Vec<u8>,
        session_flags: SessionFlags,
    ) -> Vec<u8> {
        let mut h = Header::new_request(Command::SessionSetup);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;
        h.session_id = session_id;

        let body = SessionSetupResponse {
            session_flags,
            security_buffer,
        };

        pack_message(&h, &body)
    }

    /// Build a minimal NTLM challenge message (Type 2).
    ///
    /// This is a stripped-down challenge that the NtlmAuthenticator can parse.
    fn build_ntlm_challenge() -> Vec<u8> {
        let mut buf = Vec::new();

        // Signature (8 bytes)
        buf.extend_from_slice(b"NTLMSSP\0");
        // MessageType = 2 (4 bytes)
        buf.extend_from_slice(&2u32.to_le_bytes());
        // TargetNameFields: Len=0, MaxLen=0, Offset=56
        buf.extend_from_slice(&0u16.to_le_bytes()); // Len
        buf.extend_from_slice(&0u16.to_le_bytes()); // MaxLen
        buf.extend_from_slice(&56u32.to_le_bytes()); // Offset
                                                     // NegotiateFlags
        let flags: u32 = 0x0000_0001 // UNICODE
            | 0x0000_0200  // NTLM
            | 0x0008_0000  // EXTENDED_SESSIONSECURITY
            | 0x0080_0000  // TARGET_INFO
            | 0x2000_0000  // 128
            | 0x4000_0000  // KEY_EXCH
            | 0x8000_0000  // 56
            | 0x0000_0010  // SIGN
            | 0x0000_0020; // SEAL
        buf.extend_from_slice(&flags.to_le_bytes());
        // ServerChallenge (8 bytes)
        buf.extend_from_slice(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        // Reserved (8 bytes)
        buf.extend_from_slice(&[0u8; 8]);

        // TargetInfoFields: Len, MaxLen, Offset (will be at offset 56 + target_name_len)
        // Build target info: just MsvAvEOL
        let target_info = build_av_eol();
        let ti_offset = 56u32; // right after the fixed header
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes()); // Len
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes()); // MaxLen
        buf.extend_from_slice(&ti_offset.to_le_bytes()); // Offset

        // Ensure we're at offset 56 (pad if needed).
        while buf.len() < 56 {
            buf.push(0);
        }

        // Target info data
        buf.extend_from_slice(&target_info);

        buf
    }

    /// Build an AV_PAIR list with just MsvAvEOL.
    fn build_av_eol() -> Vec<u8> {
        let mut buf = Vec::new();
        // MsvAvEOL: AvId=0, AvLen=0
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    }

    #[tokio::test]
    async fn session_setup_stores_session_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let session_id = SessionId(0xDEAD_BEEF);

        // Queue the two session setup responses.
        let challenge = build_ntlm_challenge();
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            challenge,
            SessionFlags(0),
        ));
        mock.queue_response(build_session_setup_response(
            NtStatus::SUCCESS,
            session_id,
            vec![],
            SessionFlags(0),
        ));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // Set up negotiate params (pretend we already negotiated).
        // We need to call negotiate or set params manually.
        // Let's also queue a negotiate response first.
        // Actually, let's set params directly.
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        let session = Session::setup(&mut conn, "user", "pass", "").await.unwrap();
        assert_eq!(session.session_id, session_id);
    }

    #[tokio::test]
    async fn session_setup_derives_signing_key() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let session_id = SessionId(0x1234);

        let challenge = build_ntlm_challenge();
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            challenge,
            SessionFlags(0),
        ));
        mock.queue_response(build_session_setup_response(
            NtStatus::SUCCESS,
            session_id,
            vec![],
            SessionFlags(0),
        ));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        let session = Session::setup(&mut conn, "user", "pass", "").await.unwrap();
        assert!(!session.signing_key.is_empty());
    }

    #[tokio::test]
    async fn session_setup_activates_signing() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let session_id = SessionId(0x5678);

        let challenge = build_ntlm_challenge();
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            challenge,
            SessionFlags(0),
        ));
        mock.queue_response(build_session_setup_response(
            NtStatus::SUCCESS,
            session_id,
            vec![],
            SessionFlags(0),
        ));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        let session = Session::setup(&mut conn, "user", "pass", "").await.unwrap();
        assert!(session.should_sign);
        assert_eq!(session.signing_algorithm, SigningAlgorithm::HmacSha256);
    }

    #[tokio::test]
    async fn session_setup_error_on_auth_failure() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let session_id = SessionId(0x9999);

        let challenge = build_ntlm_challenge();
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            challenge,
            SessionFlags(0),
        ));
        // Auth fails on second round.
        mock.queue_response(build_session_setup_response(
            NtStatus::LOGON_FAILURE,
            session_id,
            vec![],
            SessionFlags(0),
        ));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        let result = Session::setup(&mut conn, "user", "badpass", "").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(
                err,
                Error::Protocol {
                    status: NtStatus::LOGON_FAILURE,
                    ..
                }
            ),
            "expected LOGON_FAILURE, got: {err}"
        );
    }

    /// Queue the two NTLM rounds of a login the server accepts as `session_id`.
    fn queue_accepted_login(mock: &MockTransport, session_id: SessionId, final_status: NtStatus) {
        mock.queue_response(build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            build_ntlm_challenge(),
            SessionFlags(0),
        ));
        mock.queue_response(build_session_setup_response(
            final_status,
            session_id,
            vec![],
            SessionFlags(0),
        ));
    }

    /// The `PreviousSessionId` each SESSION_SETUP the mock saw carried.
    fn announced_previous_session_ids(mock: &MockTransport) -> Vec<u64> {
        mock.sent_messages()
            .iter()
            .map(|sent| {
                let mut cursor = ReadCursor::new(sent);
                let header = Header::unpack(&mut cursor).unwrap();
                assert_eq!(header.command, Command::SessionSetup);
                SessionSetupRequest::unpack(&mut cursor)
                    .unwrap()
                    .previous_session_id
            })
            .collect()
    }

    /// A consumer replacing a client after a drop seeds the dead session's id:
    /// both rounds announce it (MS-SMB2 § 3.2.4.2.3), and once the new session
    /// is up it's retired, so a later setup can't point at it again.
    #[tokio::test]
    async fn a_seeded_previous_session_id_rides_every_round_and_retires_on_success() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        queue_accepted_login(&mock, SessionId(0x2222), NtStatus::SUCCESS);
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        conn.set_previous_session_id(SessionId(0x1111));
        Session::setup(&mut conn, "user", "pass", "").await.unwrap();

        assert_eq!(announced_previous_session_ids(&mock), [0x1111, 0x1111]);
        assert_eq!(conn.previous_session_id(), SessionId(0));
    }

    /// A refused login leaves the seed in place, so the retry with the right
    /// password still tells the server which session it replaces.
    #[tokio::test]
    async fn a_refused_login_keeps_the_seeded_previous_session_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        queue_accepted_login(&mock, SessionId(0x2222), NtStatus::LOGON_FAILURE);
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params(&mut conn, Dialect::Smb2_0_2);

        conn.set_previous_session_id(SessionId(0x1111));
        Session::setup(&mut conn, "user", "wrong", "")
            .await
            .unwrap_err();

        assert_eq!(announced_previous_session_ids(&mock), [0x1111, 0x1111]);
        assert_eq!(conn.previous_session_id(), SessionId(0x1111));
    }

    /// Helper: put a connection in the state NEGOTIATE leaves it in.
    ///
    /// That includes a credit window: the NEGOTIATE response is what opens
    /// one, and SESSION_SETUP spends from it like any other request.
    fn set_test_params(conn: &mut Connection, dialect: Dialect) {
        set_test_params_with(conn, dialect, false);
    }

    fn set_test_params_with(conn: &mut Connection, dialect: Dialect, gmac_negotiated: bool) {
        conn.set_credits(512);
        conn.set_test_params(NegotiatedParams {
            dialect,
            max_read_size: 65536,
            max_write_size: 65536,
            max_transact_size: 65536,
            server_guid: Guid::ZERO,
            signing_required: false,
            capabilities: Capabilities::default(),
            gmac_negotiated,
            cipher: None,
            compression_supported: false,
        });
    }

    // ── The final SESSION_SETUP response has to prove itself ──
    //
    // Everything below plays the server's half of NTLM well enough to sign
    // the final response the way a real server does, then lets a test decide
    // what reaches the client: that response, an unsigned one, or a signed
    // one an on-path attacker edited afterwards.

    /// What the "wire" does to the server's final response.
    enum Final {
        /// The server signs it and it arrives intact.
        Signed,
        /// It arrives without a signature (and without `SMB2_FLAGS_SIGNED`).
        Unsigned,
        /// The server signs it, then something on the path edits it.
        SignedThenTampered(fn(&mut Vec<u8>)),
    }

    const USER: &str = "user";
    const PASS: &str = "pass";

    /// Run `Session::setup` against a mock that answers the final round with
    /// `flags`, signed (or not) with the key a real server would derive.
    async fn setup_against_server(
        dialect: Dialect,
        gmac: bool,
        username: &str,
        password: &str,
        flags: SessionFlags,
        finish: Final,
    ) -> Result<Session> {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let session_id = SessionId(0x4242);

        let mut challenge = build_session_setup_response(
            NtStatus::MORE_PROCESSING_REQUIRED,
            session_id,
            build_ntlm_challenge(),
            SessionFlags(0),
        );
        mock.queue_response(challenge.clone());

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        set_test_params_with(&mut conn, dialect, gmac);
        let mut hasher = conn.preauth_hasher();

        let (user, pass) = (username.to_string(), password.to_string());
        let task = tokio::spawn(async move { Session::setup(&mut conn, &user, &pass, "").await });

        // Wait for the AUTHENTICATE request: the key the final response is
        // signed with depends on what the client put in it.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while mock.sent_count() < 2 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "client never sent AUTHENTICATE"
            );
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let negotiate = mock.sent_message(0).unwrap();
        let authenticate = mock.sent_message(1).unwrap();
        let msg_id = |frame: &[u8]| u64::from_le_bytes(frame[24..32].try_into().unwrap());

        // The client hashed the challenge as it arrived, with the MessageId
        // the mock rewrote into it.
        challenge[24..32].copy_from_slice(&msg_id(&negotiate).to_le_bytes());
        hasher.update(&negotiate);
        hasher.update(&challenge);
        hasher.update(&authenticate);

        let request = SessionSetupRequest::unpack(&mut ReadCursor::new(&authenticate[64..]))
            .expect("AUTHENTICATE request parses");
        let session_key = crate::auth::ntlm::exported_session_key_as_server(
            &request.security_buffer,
            username,
            password,
            "",
        );
        let signing_key = match dialect {
            Dialect::Smb3_0 | Dialect::Smb3_0_2 => {
                derive_session_keys(&session_key, dialect, None, 128).signing_key
            }
            Dialect::Smb3_1_1 => {
                derive_session_keys(&session_key, dialect, Some(hasher.value()), 128).signing_key
            }
            _ => session_key,
        };
        let algorithm = algorithm_for_dialect(dialect, gmac);

        let mut response =
            build_session_setup_response(NtStatus::SUCCESS, session_id, vec![], flags);
        // Stamp the real MessageId before signing: it's under the signature,
        // and under the GMAC nonce.
        let final_id = msg_id(&authenticate);
        response[24..32].copy_from_slice(&final_id.to_le_bytes());
        if !matches!(finish, Final::Unsigned) {
            let mut header_flags = crate::types::flags::HeaderFlags(u32::from_le_bytes(
                response[16..20].try_into().unwrap(),
            ));
            header_flags.set_signed();
            response[16..20].copy_from_slice(&header_flags.0.to_le_bytes());
            crate::crypto::signing::sign_message_as_server(
                &mut response,
                &signing_key,
                algorithm,
                final_id,
                false,
            )
            .unwrap();
        }
        if let Final::SignedThenTampered(tamper) = finish {
            tamper(&mut response);
        }
        mock.queue_response(response);

        task.await.unwrap()
    }

    /// Offset of `SessionFlags` in a SESSION_SETUP response: the 64-byte
    /// header, then `StructureSize` (2).
    const SESSION_FLAGS_AT: usize = 66;

    /// The login failed because the final response didn't prove itself.
    #[track_caller]
    fn assert_signature_refused(result: Result<Session>, expected: SignatureFailure) {
        match result {
            Err(Error::SignatureInvalid {
                command: Command::SessionSetup,
                reason,
                ..
            }) if reason == expected => {}
            Err(other) => panic!("expected a {expected:?} SESSION_SETUP signature, got: {other:?}"),
            Ok(session) => panic!(
                "expected a {expected:?} SESSION_SETUP signature, got a session (sign={}, encrypt={})",
                session.should_sign, session.should_encrypt
            ),
        }
    }

    /// The login failed because a guest (or, with `anonymous`, a null)
    /// session stood in for the account it named.
    #[track_caller]
    fn assert_guest_refused(result: Result<Session>, account: &str, anonymous: bool) {
        match result {
            Err(Error::GuestSessionRefused {
                username,
                anonymous: got,
            }) if username == account && got == anonymous => {}
            Err(other) => panic!(
                "expected GuestSessionRefused for {account:?} (anonymous={anonymous}), got: {other:?}"
            ),
            Ok(session) => panic!(
                "expected GuestSessionRefused for {account:?}, got a session (sign={}, encrypt={})",
                session.should_sign, session.should_encrypt
            ),
        }
    }

    #[tokio::test]
    async fn signed_final_response_is_accepted_on_3_1_1_gmac() {
        let session = setup_against_server(
            Dialect::Smb3_1_1,
            true,
            USER,
            PASS,
            SessionFlags(0),
            Final::Signed,
        )
        .await
        .unwrap();
        assert!(session.should_sign);
        assert_eq!(session.signing_algorithm, SigningAlgorithm::AesGmac);
    }

    #[tokio::test]
    async fn signed_final_response_is_accepted_on_3_0_2() {
        let session = setup_against_server(
            Dialect::Smb3_0_2,
            false,
            USER,
            PASS,
            SessionFlags(SessionFlags::ENCRYPT_DATA),
            Final::Signed,
        )
        .await
        .unwrap();
        assert!(session.should_encrypt);
    }

    #[tokio::test]
    async fn signed_final_response_is_accepted_on_2_0_2() {
        let session = setup_against_server(
            Dialect::Smb2_0_2,
            false,
            USER,
            PASS,
            SessionFlags(0),
            Final::Signed,
        )
        .await
        .unwrap();
        assert!(session.should_sign);
    }

    /// MS-SMB2 § 3.2.5.3.1: on 3.1.1 a final response without
    /// `SMB2_FLAGS_SIGNED` is an error. Accepting it would let whoever
    /// stripped the signature choose the session's flags.
    #[tokio::test]
    async fn unsigned_final_response_is_refused_on_3_1_1() {
        let result = setup_against_server(
            Dialect::Smb3_1_1,
            false,
            USER,
            PASS,
            SessionFlags(0),
            Final::Unsigned,
        )
        .await;
        assert_signature_refused(result, SignatureFailure::Missing);
    }

    /// A server MUST sign the final response of a non-guest session on every
    /// 3.x dialect (MS-SMB2 § 3.3.5.5.3), and on 3.0 it's the only thing
    /// vouching for `ENCRYPT_DATA`.
    #[tokio::test]
    async fn unsigned_final_response_is_refused_on_3_0_2() {
        let result = setup_against_server(
            Dialect::Smb3_0_2,
            false,
            USER,
            PASS,
            SessionFlags(0),
            Final::Unsigned,
        )
        .await;
        assert_signature_refused(result, SignatureFailure::Missing);
    }

    /// The downgrade this check exists for: a server demands encryption, and
    /// an on-path attacker clears `ENCRYPT_DATA` so the session runs in the
    /// clear.
    #[tokio::test]
    async fn cleared_encrypt_data_flag_is_refused() {
        let result = setup_against_server(
            Dialect::Smb3_1_1,
            true,
            USER,
            PASS,
            SessionFlags(SessionFlags::ENCRYPT_DATA),
            Final::SignedThenTampered(|r| {
                r[SESSION_FLAGS_AT] &= !(SessionFlags::ENCRYPT_DATA as u8)
            }),
        )
        .await;
        assert_signature_refused(result, SignatureFailure::Mismatch);
    }

    /// Setting `IS_GUEST` on a genuine response turns signing off, so a
    /// credentialed login never accepts it, signed or not.
    #[tokio::test]
    async fn tampered_guest_flag_is_refused() {
        let result = setup_against_server(
            Dialect::Smb3_1_1,
            true,
            USER,
            PASS,
            SessionFlags(0),
            Final::SignedThenTampered(|r| r[SESSION_FLAGS_AT] |= SessionFlags::IS_GUEST as u8),
        )
        .await;
        assert_guest_refused(result, USER, false);
    }

    /// Samba's `map to guest = bad user` answers a wrong password with a
    /// guest session. Someone who typed a password wants to hear it was wrong.
    #[tokio::test]
    async fn guest_session_for_a_credentialed_login_is_refused() {
        for dialect in [Dialect::Smb2_0_2, Dialect::Smb3_1_1] {
            let result = setup_against_server(
                dialect,
                false,
                USER,
                PASS,
                SessionFlags(SessionFlags::IS_GUEST),
                Final::Unsigned,
            )
            .await;
            assert_guest_refused(result, USER, false);
        }
    }

    #[tokio::test]
    async fn null_session_for_a_credentialed_login_is_refused() {
        let result = setup_against_server(
            Dialect::Smb3_1_1,
            false,
            USER,
            PASS,
            SessionFlags(SessionFlags::IS_NULL),
            Final::Unsigned,
        )
        .await;
        assert_guest_refused(result, USER, true);
    }

    /// 2.x servers aren't required to flag the final response signed from
    /// the client's point of view, but one that says it signed has to mean it.
    #[tokio::test]
    async fn bad_signature_is_refused_on_2_0_2() {
        let result = setup_against_server(
            Dialect::Smb2_0_2,
            false,
            USER,
            PASS,
            SessionFlags(0),
            Final::SignedThenTampered(|r| r[48] ^= 0xFF),
        )
        .await;
        assert_signature_refused(result, SignatureFailure::Mismatch);
    }

    /// What the caller asked for: no username means a guest or anonymous
    /// session, and the server granting one is the success case.
    #[tokio::test]
    async fn guest_login_without_a_username_still_works() {
        for dialect in [Dialect::Smb2_0_2, Dialect::Smb3_1_1] {
            let session = setup_against_server(
                dialect,
                false,
                "",
                "",
                SessionFlags(SessionFlags::IS_GUEST),
                Final::Unsigned,
            )
            .await
            .unwrap();
            assert!(!session.should_sign);
        }
    }

    /// `Guest` (any ASCII case) asks for a guest session as plainly as an
    /// empty name does, so a server granting one is the success case, not a
    /// downgrade. Consumers have logged in to guest shares this way for years,
    /// and some servers accept the named `Guest` login but refuse an anonymous
    /// one.
    #[tokio::test]
    async fn a_login_named_guest_answered_as_guest_succeeds() {
        for name in ["Guest", "GUEST", "guest"] {
            for dialect in [Dialect::Smb2_0_2, Dialect::Smb3_1_1] {
                for flags in [SessionFlags::IS_GUEST, SessionFlags::IS_NULL] {
                    let session = setup_against_server(
                        dialect,
                        false,
                        name,
                        "",
                        SessionFlags(flags),
                        Final::Unsigned,
                    )
                    .await
                    .unwrap_or_else(|e| panic!("{name} on {dialect:?} answered {flags:#x}: {e}"));
                    assert!(!session.should_sign);
                }
            }
        }
    }

    /// Only the name `Guest` itself is guest-on-purpose: an account that
    /// merely looks like it is an account, and a guest answer to it is still
    /// the downgrade (or the wrong password) it always was.
    #[tokio::test]
    async fn a_login_merely_like_guest_answered_as_guest_is_still_refused() {
        for name in ["guest1", "Guests", " Guest", "DOMAIN\\Guest"] {
            let result = setup_against_server(
                Dialect::Smb3_1_1,
                false,
                name,
                "",
                SessionFlags(SessionFlags::IS_GUEST),
                Final::Unsigned,
            )
            .await;
            assert_guest_refused(result, name, false);
        }
    }

    /// A `Guest` login the server answers with a full session (a Windows
    /// `Guest` account that's enabled) is an account login from there on: its
    /// final response still has to verify.
    #[tokio::test]
    async fn a_login_named_guest_answered_with_a_full_session_is_still_verified() {
        let result = setup_against_server(
            Dialect::Smb3_1_1,
            false,
            "Guest",
            "",
            SessionFlags(0),
            Final::SignedThenTampered(|r| r[48] ^= 0xFF),
        )
        .await;
        assert_signature_refused(result, SignatureFailure::Mismatch);

        let session = setup_against_server(
            Dialect::Smb3_1_1,
            false,
            "Guest",
            "",
            SessionFlags(0),
            Final::Signed,
        )
        .await
        .unwrap();
        assert!(session.should_sign);
    }

    #[test]
    fn a_sessions_debug_output_hides_its_keys() {
        let session = Session {
            session_id: SessionId(7),
            signing_key: vec![0xAB; 16],
            encryption_key: Some(vec![0xAB; 16]),
            decryption_key: None,
            signing_algorithm: SigningAlgorithm::AesCmac,
            should_sign: true,
            should_encrypt: true,
        };
        let printed = format!("{session:?}");
        assert!(!printed.contains("171"), "{printed}");
        assert!(printed.contains("encryption_key: Some(***)"), "{printed}");
        assert!(printed.contains("decryption_key: None"), "{printed}");
        assert!(printed.contains("should_encrypt: true"), "{printed}");
    }
}
