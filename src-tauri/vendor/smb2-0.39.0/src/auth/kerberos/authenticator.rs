//! Stateful Kerberos authenticator for SMB2 session setup.
//!
//! Performs the full Kerberos authentication exchange:
//! 1. AS exchange (client -> KDC): get a TGT
//! 2. TGS exchange (client -> KDC): get a service ticket for `cifs/hostname`
//! 3. AP-REQ construction: wrap the service ticket for SESSION_SETUP
//!
//! After [`KerberosAuthenticator::authenticate`] succeeds, call
//! [`token()`](KerberosAuthenticator::token) for the SPNEGO-wrapped AP-REQ
//! and [`session_key()`](KerberosAuthenticator::session_key) for the SMB
//! session key.

use log::{debug, trace};
use std::time::Duration;

use crate::auth::der::{der_tlv, parse_der_tlv};
use crate::auth::kerberos::crypto::{
    compute_checksum, etype_from_i32, kerberos_decrypt, kerberos_encrypt, string_to_key_aes,
    string_to_key_rc4, EncryptionType,
};
use crate::auth::kerberos::kdc::{send_to_kdc, KdcConfig};
use crate::auth::kerberos::messages::{
    encode_ap_req, encode_as_req, encode_authenticator, encode_pa_enc_timestamp, encode_tgs_req,
    encode_tgs_req_body, parse_enc_kdc_rep_part, parse_kdc_rep, parse_krb_error, EncryptedData,
    PaData, PrincipalName, Ticket,
};
use crate::auth::spnego::{wrap_neg_token_init, OID_KERBEROS, OID_MS_KERBEROS};
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Key usage numbers (RFC 4120 section 7.5.1)
// ---------------------------------------------------------------------------

/// Key usage for PA-ENC-TIMESTAMP encryption.
const KEY_USAGE_PA_ENC_TIMESTAMP: u32 = 1;

/// Key usage for AS-REP EncKDCRepPart decryption.
const KEY_USAGE_AS_REP_ENC_PART: u32 = 3;

/// Key usage for AP-REQ Authenticator encryption (standard, RFC 4120).
///
/// Used for the PA-TGS-REQ authenticator in TGS exchanges.
const KEY_USAGE_AP_REQ_AUTHENTICATOR: u32 = 7;

/// Key usage for AP-REQ Authenticator encryption (MS-KILE/SPNEGO).
///
/// Windows servers expect key usage 11 for the AP-REQ Authenticator
/// in SPNEGO-wrapped SMB SESSION_SETUP exchanges. Impacket uses this.
const KEY_USAGE_AP_REQ_AUTHENTICATOR_SPNEGO: u32 = 11;

/// Key usage for TGS-REP EncKDCRepPart decryption (sub-session key).
///
/// Per RFC 4120 section 7.5.1 and MS-KILE, the TGS-REP enc-part is
/// encrypted with key usage 8 when using the TGT session key.
/// However, some implementations use key usage 9. We try 8 first,
/// then fall back to 9 if decryption fails.
const KEY_USAGE_TGS_REP_ENC_PART_SESSION_KEY: u32 = 8;

/// Fallback key usage for TGS-REP (some KDCs use 9).
const KEY_USAGE_TGS_REP_ENC_PART_SUBKEY: u32 = 9;

// ---------------------------------------------------------------------------
// KDC error codes (RFC 4120 section 7.5.9)
// ---------------------------------------------------------------------------

/// KDC_ERR_PREAUTH_REQUIRED: pre-authentication information was needed but
/// not found in the request.
const KDC_ERR_PREAUTH_REQUIRED: i32 = 25;

// ---------------------------------------------------------------------------
// PA-DATA type constants
// ---------------------------------------------------------------------------

/// PA-ENC-TIMESTAMP (padata type 2).
const PA_ENC_TIMESTAMP: i32 = 2;

/// PA-ETYPE-INFO2 (padata type 19).
const PA_ETYPE_INFO2: i32 = 19;

/// PA-PAC-REQUEST (padata type 128).
const PA_PAC_REQUEST: i32 = 128;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Credentials for Kerberos authentication.
///
/// `Debug` shows the password as `***`.
#[derive(Clone)]
pub struct KerberosCredentials {
    /// Username (without realm).
    pub username: String,
    /// Password.
    pub password: String,
    /// Kerberos realm (uppercase, for example, "CORP.EXAMPLE.COM").
    pub realm: String,
    /// KDC address (host:port or host, port defaults to 88).
    pub kdc_address: String,
}

impl std::fmt::Debug for KerberosCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KerberosCredentials")
            .field("username", &self.username)
            .field("password", &crate::crypto::Redacted)
            .field("realm", &self.realm)
            .field("kdc_address", &self.kdc_address)
            .finish()
    }
}

/// Stateful Kerberos authenticator.
///
/// Performs the full Kerberos exchange: AS -> TGS -> AP.
/// After completion, [`session_key()`](Self::session_key) returns the session
/// key for SMB signing/encryption.
pub struct KerberosAuthenticator {
    credentials: KerberosCredentials,
    /// TGT obtained from the AS exchange.
    tgt: Option<Ticket>,
    /// Session key from the AS exchange (used to authenticate to the TGS).
    as_session_key: Option<Vec<u8>>,
    /// Service ticket obtained from the TGS exchange.
    service_ticket: Option<Ticket>,
    /// Session key from the TGS exchange (the SMB session key).
    tgs_session_key: Option<Vec<u8>>,
    /// SPNEGO-wrapped AP-REQ bytes for SESSION_SETUP.
    ap_req_bytes: Option<Vec<u8>>,
    /// Final session key for SMB (same as tgs_session_key).
    session_key: Option<Vec<u8>>,
    /// Negotiated encryption type.
    etype: EncryptionType,
}

impl KerberosAuthenticator {
    /// Create a new authenticator with the given credentials.
    pub fn new(credentials: KerberosCredentials) -> Self {
        Self {
            credentials,
            tgt: None,
            as_session_key: None,
            service_ticket: None,
            tgs_session_key: None,
            ap_req_bytes: None,
            session_key: None,
            etype: EncryptionType::Aes256CtsHmacSha196,
        }
    }

    /// Perform the full Kerberos exchange (AS + TGS + build AP-REQ).
    ///
    /// After this returns `Ok(())`, call [`token()`](Self::token) to get the
    /// SPNEGO-wrapped AP-REQ for SESSION_SETUP, and
    /// [`session_key()`](Self::session_key) for the session key.
    ///
    /// This is async because it contacts the KDC over the network.
    pub async fn authenticate(&mut self, server_hostname: &str) -> Result<()> {
        let kdc_config = KdcConfig {
            address: self.credentials.kdc_address.clone(),
            timeout: Duration::from_secs(10),
        };

        // ── Step 1: AS exchange ──
        debug!("kerberos: starting AS exchange");
        self.as_exchange(&kdc_config).await?;

        // ── Step 2: TGS exchange ──
        debug!("kerberos: starting TGS exchange for cifs host={server_hostname:?}");
        self.tgs_exchange(&kdc_config, server_hostname).await?;

        // ── Step 3: Build AP-REQ ──
        debug!("kerberos: building AP-REQ");
        self.build_ap_req()?;

        debug!("kerberos: authentication complete");
        Ok(())
    }

    /// Authenticate using a cached credential from a ccache file.
    ///
    /// If the ccache has a service ticket for `cifs/<server_hostname>`, uses it
    /// directly (no KDC contact needed). If only a TGT is cached, performs a
    /// TGS exchange to get the service ticket.
    ///
    /// After this returns `Ok(())`, call [`token()`](Self::token) and
    /// [`session_key()`](Self::session_key) as usual.
    pub async fn authenticate_from_ccache(
        &mut self,
        ccache: &crate::auth::kerberos::ccache::CCache,
        server_hostname: &str,
    ) -> Result<()> {
        let realm = &self.credentials.realm;

        // Try cached service ticket first (no KDC needed).
        if let Some(svc) = ccache.find_service_ticket("cifs", server_hostname, realm) {
            debug!("kerberos: using cached service ticket for cifs host={server_hostname:?}");
            self.load_service_ticket_from_ccache(svc)?;
            self.build_ap_req()?;
            debug!("kerberos: authentication complete (from cached service ticket)");
            return Ok(());
        }

        // Fall back to cached TGT + TGS exchange.
        if let Some(tgt_cred) = ccache.find_tgt(realm) {
            debug!(
                "kerberos: using cached TGT, doing TGS exchange for cifs host={server_hostname:?}"
            );
            self.load_tgt_from_ccache(tgt_cred)?;

            let kdc_config = KdcConfig {
                address: self.credentials.kdc_address.clone(),
                timeout: Duration::from_secs(10),
            };
            self.tgs_exchange(&kdc_config, server_hostname).await?;
            self.build_ap_req()?;
            debug!("kerberos: authentication complete (TGT from cache + TGS exchange)");
            return Ok(());
        }

        Err(Error::Auth {
            message: format!("ccache has no TGT or service ticket for realm {realm}"),
        })
    }

    /// Load a service ticket from a ccache credential entry.
    fn load_service_ticket_from_ccache(
        &mut self,
        cred: &crate::auth::kerberos::ccache::CcacheCredential,
    ) -> Result<()> {
        // Parse the ticket from the raw DER bytes.
        let ticket = crate::auth::kerberos::messages::parse_ticket(&cred.ticket)?;

        // Determine the etype from the session key.
        let etype = etype_from_code(cred.key_etype as i32)?;
        self.etype = etype;

        self.service_ticket = Some(ticket);
        self.tgs_session_key = Some(cred.key_data.clone());
        self.session_key = Some(cred.key_data.clone());

        Ok(())
    }

    /// Load a TGT from a ccache credential entry.
    fn load_tgt_from_ccache(
        &mut self,
        cred: &crate::auth::kerberos::ccache::CcacheCredential,
    ) -> Result<()> {
        let ticket = crate::auth::kerberos::messages::parse_ticket(&cred.ticket)?;

        let etype = etype_from_code(cred.key_etype as i32)?;
        self.etype = etype;

        self.tgt = Some(ticket);
        self.as_session_key = Some(cred.key_data.clone());

        Ok(())
    }

    /// Get the SPNEGO-wrapped AP-REQ token for SESSION_SETUP.
    ///
    /// Available after [`authenticate()`](Self::authenticate) succeeds.
    pub fn token(&self) -> Option<&[u8]> {
        self.ap_req_bytes.as_deref()
    }

    /// Get the session key for SMB signing/encryption.
    ///
    /// Available after [`authenticate()`](Self::authenticate) succeeds.
    pub fn session_key(&self) -> Option<&[u8]> {
        self.session_key.as_deref()
    }

    // =====================================================================
    // AS exchange
    // =====================================================================

    /// Perform the AS exchange to get a TGT.
    async fn as_exchange(&mut self, kdc_config: &KdcConfig) -> Result<()> {
        let realm = &self.credentials.realm;
        let username = &self.credentials.username;

        // Client principal: username@REALM
        let cname = PrincipalName {
            name_type: 1, // KRB_NT_PRINCIPAL
            name_string: vec![username.clone()],
        };

        // Service principal for TGT: krbtgt/REALM
        let sname = PrincipalName {
            name_type: 2, // KRB_NT_SRV_INST
            name_string: vec!["krbtgt".to_string(), realm.clone()],
        };

        // Generate a random nonce.
        let nonce = generate_nonce();

        // Requested etypes: prefer AES-256, then AES-128, then RC4.
        let etypes = [
            EncryptionType::Aes256CtsHmacSha196,
            EncryptionType::Aes128CtsHmacSha196,
            EncryptionType::Rc4Hmac,
        ];

        // First attempt: send AS-REQ without pre-authentication.
        // Most KDCs will respond with KDC_ERR_PREAUTH_REQUIRED.
        let as_req = encode_as_req(&cname, realm, &sname, nonce, &etypes, &[]);
        let response = send_to_kdc(kdc_config, &as_req).await?;

        // Check if we got a KRB-ERROR (APPLICATION [30] = 0x7e).
        trace!(
            "kerberos: AS response first 32 bytes: {:02x?}",
            &response[..response.len().min(32)]
        );
        let response = if !response.is_empty() && response[0] == 0x7e {
            let krb_error = parse_krb_error(&response)?;

            if krb_error.error_code == KDC_ERR_PREAUTH_REQUIRED {
                debug!("kerberos: got KDC_ERR_PREAUTH_REQUIRED, retrying with pre-authentication");

                // Extract supported etypes from e-data if available.
                let chosen_etype = if let Some(ref e_data) = krb_error.e_data {
                    self.extract_best_etype(e_data).unwrap_or(self.etype)
                } else {
                    self.etype
                };
                self.etype = chosen_etype;

                // Derive the user's long-term key from the password.
                let user_key = self.derive_user_key();

                // Build PA-ENC-TIMESTAMP.
                let (ctime, cusec) = current_kerberos_time();
                let timestamp_plaintext = encode_pa_enc_timestamp(&ctime, cusec);
                let encrypted_timestamp = kerberos_encrypt(
                    &user_key,
                    KEY_USAGE_PA_ENC_TIMESTAMP,
                    &timestamp_plaintext,
                    self.etype,
                );

                let enc_timestamp_data = EncryptedData {
                    etype: self.etype as i32,
                    kvno: None,
                    cipher: encrypted_timestamp,
                };
                let pa_enc_ts_value = encode_encrypted_data_raw(&enc_timestamp_data);

                // Build PA-PAC-REQUEST (request the PAC).
                let pa_pac_value = encode_pa_pac_request(true);

                let padata = vec![
                    PaData {
                        padata_type: PA_ENC_TIMESTAMP,
                        padata_value: pa_enc_ts_value,
                    },
                    PaData {
                        padata_type: PA_PAC_REQUEST,
                        padata_value: pa_pac_value,
                    },
                ];

                // Retry AS-REQ with pre-authentication.
                let as_req = encode_as_req(&cname, realm, &sname, nonce, &etypes, &padata);
                send_to_kdc(kdc_config, &as_req).await?
            } else {
                return Err(Error::Auth {
                    message: format!(
                        "Kerberos AS exchange failed: KRB-ERROR code {} ({})",
                        krb_error.error_code,
                        krb_error.e_text.unwrap_or_default()
                    ),
                });
            }
        } else {
            response
        };

        // Check for error in the response to the pre-auth attempt.
        if !response.is_empty() && response[0] == 0x7e {
            let krb_error = parse_krb_error(&response)?;
            return Err(Error::Auth {
                message: format!(
                    "Kerberos AS exchange failed: KRB-ERROR code {} ({})",
                    krb_error.error_code,
                    krb_error.e_text.unwrap_or_default()
                ),
            });
        }

        // Parse AS-REP (APPLICATION [11] = 0x6b).
        let as_rep = parse_kdc_rep(&response)?;
        if as_rep.msg_type != 11 {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected AS-REP (msg_type 11), got {}",
                as_rep.msg_type
            )));
        }

        // Update etype from what the KDC actually chose.
        self.etype = etype_from_i32(as_rep.enc_part.etype)?;
        trace!(
            "kerberos: AS-REP etype={}, kvno={:?}, cipher_len={}, crealm={:?}, cname={:?}",
            as_rep.enc_part.etype,
            as_rep.enc_part.kvno,
            as_rep.enc_part.cipher.len(),
            as_rep.crealm,
            as_rep.cname.name_string,
        );

        // Derive the user's long-term key (may have been derived already,
        // but etype might have changed based on the KDC response).
        let user_key = self.derive_user_key();
        trace!(
            "kerberos: user_key len={}, etype={:?}, salt from realm={:?} user={:?}",
            user_key.len(),
            self.etype,
            self.credentials.realm,
            self.credentials.username,
        );

        // Decrypt the enc-part to get the session key.
        let enc_part_plain = kerberos_decrypt(
            &user_key,
            KEY_USAGE_AS_REP_ENC_PART,
            &as_rep.enc_part.cipher,
            self.etype,
        )?;

        let enc_kdc_rep = parse_enc_kdc_rep_part(&enc_part_plain)?;

        trace!(
            "kerberos: AS session key type={}, len={}",
            enc_kdc_rep.key.keytype,
            enc_kdc_rep.key.keyvalue.len()
        );

        self.tgt = Some(as_rep.ticket);
        self.as_session_key = Some(enc_kdc_rep.key.keyvalue);

        Ok(())
    }

    // =====================================================================
    // TGS exchange
    // =====================================================================

    /// Perform the TGS exchange to get a service ticket.
    async fn tgs_exchange(&mut self, kdc_config: &KdcConfig, server_hostname: &str) -> Result<()> {
        let tgt = self
            .tgt
            .as_ref()
            .ok_or_else(|| Error::Auth {
                message: "TGS exchange requires a TGT (run AS exchange first)".to_string(),
            })?
            .clone();
        let as_session_key = self
            .as_session_key
            .as_ref()
            .ok_or_else(|| Error::Auth {
                message: "TGS exchange requires AS session key".to_string(),
            })?
            .clone();

        let realm = &self.credentials.realm;
        let username = &self.credentials.username;

        // Service principal: cifs/server_hostname
        let sname = PrincipalName {
            name_type: 2, // KRB_NT_SRV_INST
            name_string: vec!["cifs".to_string(), server_hostname.to_string()],
        };

        // Build an AP-REQ wrapping the TGT for the TGS (PA-TGS-REQ).
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec![username.clone()],
        };

        let nonce = generate_nonce();
        // Request etypes in preference order. The KDC picks the session key
        // type from this list. AES-256 preferred, with AES-128 and RC4 fallback.
        let etypes = [
            EncryptionType::Aes256CtsHmacSha196,
            EncryptionType::Aes128CtsHmacSha196,
            EncryptionType::Rc4Hmac,
        ];

        // Build the KDC-REQ-BODY first, so we can compute a checksum
        // over it for the Authenticator (required per RFC 4120 section 7.2.2).
        let req_body = encode_tgs_req_body(realm, &sname, nonce, &etypes);

        // Compute checksum over KDC-REQ-BODY using key usage 6
        // (PA-TGS-REQ padata AP-REQ Authenticator cksum).
        let body_checksum = compute_checksum(&as_session_key, 6, &req_body, self.etype);
        let checksum_type: i32 = match self.etype {
            EncryptionType::Aes256CtsHmacSha196 => 16, // hmac-sha1-96-aes256
            EncryptionType::Aes128CtsHmacSha196 => 15, // hmac-sha1-96-aes128
            EncryptionType::Rc4Hmac => -138,           // HMAC_MD5 (KERB_CHECKSUM_HMAC_MD5)
        };

        let (ctime, cusec) = current_kerberos_time();
        let authenticator_plain = encode_authenticator(
            realm,
            &cname,
            &ctime,
            cusec,
            None,
            None,
            Some((&body_checksum, checksum_type)),
        );

        trace!(
            "kerberos: TGS authenticator plain ({} bytes), session key len={}",
            authenticator_plain.len(),
            as_session_key.len()
        );

        let encrypted_authenticator = kerberos_encrypt(
            &as_session_key,
            KEY_USAGE_AP_REQ_AUTHENTICATOR,
            &authenticator_plain,
            self.etype,
        );

        let authenticator_enc_data = EncryptedData {
            etype: self.etype as i32,
            kvno: None,
            cipher: encrypted_authenticator,
        };

        let tgt_ap_req = encode_ap_req(&tgt, &authenticator_enc_data, false);

        let tgs_req = encode_tgs_req(realm, &sname, nonce, &etypes, &tgt_ap_req);
        let response = send_to_kdc(kdc_config, &tgs_req).await?;

        // Check for KRB-ERROR.
        if !response.is_empty() && response[0] == 0x7e {
            let krb_error = parse_krb_error(&response)?;
            return Err(Error::Auth {
                message: format!(
                    "Kerberos TGS exchange failed: KRB-ERROR code {} ({})",
                    krb_error.error_code,
                    krb_error.e_text.unwrap_or_default()
                ),
            });
        }

        // Parse TGS-REP (APPLICATION [13] = 0x6d).
        let tgs_rep = parse_kdc_rep(&response)?;
        trace!(
            "kerberos: TGS-REP ticket etype={}, kvno={:?}, cipher_len={}",
            tgs_rep.ticket.enc_part.etype,
            tgs_rep.ticket.enc_part.kvno,
            tgs_rep.ticket.enc_part.cipher.len()
        );
        trace!(
            "kerberos: TGS-REP enc-part etype={}, kvno={:?}",
            tgs_rep.enc_part.etype,
            tgs_rep.enc_part.kvno
        );
        if tgs_rep.msg_type != 13 {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected TGS-REP (msg_type 13), got {}",
                tgs_rep.msg_type
            )));
        }

        // Decrypt the enc-part with the AS session key.
        // Try key usage 8 first (session key), fall back to 9 (subkey).
        let enc_part_plain = match kerberos_decrypt(
            &as_session_key,
            KEY_USAGE_TGS_REP_ENC_PART_SESSION_KEY,
            &tgs_rep.enc_part.cipher,
            self.etype,
        ) {
            Ok(plain) => plain,
            Err(_) => {
                debug!("kerberos: TGS-REP decryption with key usage 8 failed, trying 9");
                kerberos_decrypt(
                    &as_session_key,
                    KEY_USAGE_TGS_REP_ENC_PART_SUBKEY,
                    &tgs_rep.enc_part.cipher,
                    self.etype,
                )?
            }
        };

        let enc_kdc_rep = parse_enc_kdc_rep_part(&enc_part_plain)?;

        trace!(
            "kerberos: TGS session key type={}, len={}",
            enc_kdc_rep.key.keytype,
            enc_kdc_rep.key.keyvalue.len()
        );

        // Log ticket raw bytes info.
        trace!(
            "kerberos: service ticket has raw_bytes={}, raw_len={:?}",
            tgs_rep.ticket.raw_bytes.is_some(),
            tgs_rep.ticket.raw_bytes.as_ref().map(|b| b.len())
        );

        // Use the session key's actual etype for Authenticator encryption.
        let tgs_key_etype = match enc_kdc_rep.key.keytype {
            18 => EncryptionType::Aes256CtsHmacSha196,
            17 => EncryptionType::Aes128CtsHmacSha196,
            23 => EncryptionType::Rc4Hmac,
            other => {
                return Err(Error::Auth {
                    message: format!("TGS session key has unsupported etype {other}"),
                });
            }
        };
        self.etype = tgs_key_etype;

        self.service_ticket = Some(tgs_rep.ticket);
        self.tgs_session_key = Some(enc_kdc_rep.key.keyvalue.clone());
        self.session_key = Some(enc_kdc_rep.key.keyvalue);

        Ok(())
    }

    // =====================================================================
    // AP-REQ construction
    // =====================================================================

    /// Build the AP-REQ and wrap it in SPNEGO for SESSION_SETUP.
    fn build_ap_req(&mut self) -> Result<()> {
        let service_ticket = self
            .service_ticket
            .as_ref()
            .ok_or_else(|| Error::Auth {
                message: "AP-REQ requires a service ticket (run TGS exchange first)".to_string(),
            })?
            .clone();
        let tgs_session_key = self
            .tgs_session_key
            .as_ref()
            .ok_or_else(|| Error::Auth {
                message: "AP-REQ requires TGS session key".to_string(),
            })?
            .clone();

        let realm = &self.credentials.realm;
        let username = &self.credentials.username;

        let cname = PrincipalName {
            name_type: 1,
            name_string: vec![username.clone()],
        };

        // Build and encrypt the Authenticator.
        let (ctime, cusec) = current_kerberos_time();

        // Minimal Authenticator: no subkey, no seq-number, no checksum.
        // This matches impacket's working implementation. Windows accepts
        // this minimal format for SMB Kerberos authentication.
        let authenticator_plain = encode_authenticator(
            realm, &cname, &ctime, cusec, None, // no subkey
            None, // no seq-number
            None, // no checksum
        );

        let encrypted_authenticator = kerberos_encrypt(
            &tgs_session_key,
            KEY_USAGE_AP_REQ_AUTHENTICATOR_SPNEGO,
            &authenticator_plain,
            self.etype,
        );

        let authenticator_enc_data = EncryptedData {
            etype: self.etype as i32,
            kvno: None,
            cipher: encrypted_authenticator,
        };

        let ap_req = encode_ap_req(&service_ticket, &authenticator_enc_data, true);

        let gss_mech_token = wrap_gss_ap_req(&ap_req);

        // Wrap in SPNEGO NegTokenInit with MS Kerberos OID.
        let spnego_token = wrap_neg_token_init(&[OID_MS_KERBEROS], &gss_mech_token);

        // The SMB session key is the TGS session key.
        self.session_key = Some(tgs_session_key);
        self.ap_req_bytes = Some(spnego_token);

        Ok(())
    }

    /// Process the server's mutual authentication token from SPNEGO.
    ///
    /// The token may be GSS-API wrapped. After unwrapping, the 2-byte token ID
    /// tells us what it is:
    /// - `02 00`: AP-REP — contains optional server subkey
    /// - `03 00`: KRB-ERROR — logged but not fatal (session may still be valid)
    pub fn process_mutual_auth_token(&mut self, token_bytes: &[u8]) -> Result<()> {
        use crate::auth::kerberos::messages::{
            parse_ap_rep, parse_enc_ap_rep_part, parse_krb_error,
        };

        // Unwrap GSS-API APPLICATION [0] wrapper if present.
        let inner = if !token_bytes.is_empty() && token_bytes[0] == 0x60 {
            // Skip APPLICATION [0] header + OID
            let (_, gss_inner, _) =
                crate::auth::kerberos::messages::parse_gss_api_wrapper(token_bytes)?;
            gss_inner
        } else {
            token_bytes.to_vec()
        };

        if inner.len() < 2 {
            return Err(Error::invalid_data("Kerberos: mutual auth token too short"));
        }

        let token_id = [inner[0], inner[1]];
        let krb_data = &inner[2..];

        match token_id {
            [0x02, 0x00] => {
                // AP-REP
                debug!("kerberos: processing AP-REP from server");
                let ap_rep = parse_ap_rep(krb_data)?;

                const KEY_USAGE_AP_REP_ENC_PART: u32 = 12;
                let current_key = self.session_key.as_ref().ok_or_else(|| Error::Auth {
                    message: "No session key available to decrypt AP-REP".to_string(),
                })?;

                let etype = match ap_rep.enc_part.etype {
                    18 => EncryptionType::Aes256CtsHmacSha196,
                    17 => EncryptionType::Aes128CtsHmacSha196,
                    23 => EncryptionType::Rc4Hmac,
                    other => {
                        return Err(Error::Auth {
                            message: format!("AP-REP: unsupported etype {other}"),
                        })
                    }
                };

                let plain = kerberos_decrypt(
                    current_key,
                    KEY_USAGE_AP_REP_ENC_PART,
                    &ap_rep.enc_part.cipher,
                    etype,
                )?;

                let enc_part = parse_enc_ap_rep_part(&plain)?;

                if let Some(server_subkey) = enc_part.subkey {
                    trace!(
                        "kerberos: AP-REP server subkey, etype={}, len={}",
                        server_subkey.keytype,
                        server_subkey.keyvalue.len()
                    );
                    self.session_key = Some(server_subkey.keyvalue);
                } else {
                    trace!("kerberos: AP-REP has no server subkey");
                }
            }
            [0x03, 0x00] => {
                // KRB-ERROR — the server's Kerberos layer reported an error,
                // but the SMB session may still be valid. Log and continue.
                match parse_krb_error(krb_data) {
                    Ok(err) => {
                        debug!(
                            "kerberos: mutual auth KRB-ERROR code={}, realm={:?}, sname={:?}, e_text={:?}, e_data={:02x?}",
                            err.error_code, err.realm, err.sname, err.e_text,
                            err.e_data.as_deref().unwrap_or(&[])
                        );
                    }
                    Err(e) => {
                        debug!("kerberos: failed to parse KRB-ERROR in mutual auth: {}", e);
                    }
                }
            }
            _ => {
                debug!(
                    "kerberos: unexpected mutual auth token ID: {:02x} {:02x}",
                    token_id[0], token_id[1]
                );
            }
        }

        Ok(())
    }

    // =====================================================================
    // Helpers
    // =====================================================================

    /// Derive the user's long-term key from the password.
    fn derive_user_key(&self) -> Vec<u8> {
        let salt = format!("{}{}", self.credentials.realm, self.credentials.username);
        match self.etype {
            EncryptionType::Aes256CtsHmacSha196 => {
                string_to_key_aes(&self.credentials.password, &salt, 32)
            }
            EncryptionType::Aes128CtsHmacSha196 => {
                string_to_key_aes(&self.credentials.password, &salt, 16)
            }
            EncryptionType::Rc4Hmac => string_to_key_rc4(&self.credentials.password),
        }
    }

    /// Extract the best supported etype from ETYPE-INFO2 in the KRB-ERROR e-data.
    ///
    /// The e-data for KDC_ERR_PREAUTH_REQUIRED contains a METHOD-DATA
    /// (SEQUENCE OF PA-DATA). We look for PA-ETYPE-INFO2 (type 19) which
    /// contains a SEQUENCE OF ETYPE-INFO2-ENTRY.
    fn extract_best_etype(&self, e_data: &[u8]) -> Option<EncryptionType> {
        // Parse METHOD-DATA: SEQUENCE OF PA-DATA.
        // Each PA-DATA is SEQUENCE { [1] padata-type INTEGER, [2] padata-value OCTET STRING }.
        // We look for padata-type 19 (PA-ETYPE-INFO2).
        let entries = parse_method_data(e_data).ok()?;

        for entry in &entries {
            if entry.padata_type == PA_ETYPE_INFO2 {
                // Parse ETYPE-INFO2: SEQUENCE OF ETYPE-INFO2-ENTRY
                // Each entry: SEQUENCE { [0] etype INTEGER, [1] salt GeneralString OPTIONAL, ... }
                if let Some(etype) = parse_etype_info2_best(&entry.padata_value) {
                    return Some(etype);
                }
            }
        }

        None
    }
}

// =========================================================================
// DER encoding helpers for PA-DATA values
// =========================================================================

/// Encode an EncryptedData as raw DER (for embedding in PA-DATA values).
fn encode_encrypted_data_raw(ed: &EncryptedData) -> Vec<u8> {
    // EncryptedData ::= SEQUENCE {
    //   etype  [0] Int32,
    //   kvno   [1] UInt32 OPTIONAL,
    //   cipher [2] OCTET STRING
    // }
    let etype = der_context(0, &der_integer(ed.etype));
    let cipher = der_context(2, &der_octet_string(&ed.cipher));
    if let Some(kvno) = ed.kvno {
        let kvno_enc = der_context(1, &der_integer(kvno));
        der_sequence(&[&etype, &kvno_enc, &cipher])
    } else {
        der_sequence(&[&etype, &cipher])
    }
}

/// Encode a PA-PAC-REQUEST value.
///
/// KERB-PA-PAC-REQUEST ::= SEQUENCE {
///   include-pac [0] BOOLEAN
/// }
fn encode_pa_pac_request(include_pac: bool) -> Vec<u8> {
    let bool_val: &[u8] = if include_pac {
        &[0x01, 0x01, 0xff]
    } else {
        &[0x01, 0x01, 0x00]
    };
    let include = der_context(0, bool_val);
    der_sequence(&[&include])
}

// =========================================================================
// Parsing helpers for PREAUTH_REQUIRED e-data
// =========================================================================

/// Parse METHOD-DATA (SEQUENCE OF PA-DATA) from a KRB-ERROR's e-data.
fn parse_method_data(data: &[u8]) -> Result<Vec<PaData>> {
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != 0x30 {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE for METHOD-DATA, got 0x{tag:02x}"
        )));
    }

    let mut entries = Vec::new();
    let mut pos = 0;
    while pos < seq_data.len() {
        let (entry_tag, entry_data, consumed) = parse_der_tlv(&seq_data[pos..])?;
        if entry_tag == 0x30 {
            // PA-DATA SEQUENCE
            let fields = parse_sequence_fields_local(entry_data)?;
            let mut padata_type = None;
            let mut padata_value = None;
            for (ftag, fvalue) in &fields {
                match ftag {
                    0xa1 => padata_type = Some(parse_der_integer_local(fvalue)?),
                    0xa2 => padata_value = Some(parse_der_octet_string_local(fvalue)?),
                    _ => {}
                }
            }
            if let (Some(pt), Some(pv)) = (padata_type, padata_value) {
                entries.push(PaData {
                    padata_type: pt,
                    padata_value: pv,
                });
            }
        }
        pos += consumed;
    }

    Ok(entries)
}

/// Parse the best etype from an ETYPE-INFO2 value.
///
/// Returns the first etype we support, preferring AES-256 > AES-128 > RC4.
fn parse_etype_info2_best(data: &[u8]) -> Option<EncryptionType> {
    let (tag, seq_data, _) = parse_der_tlv(data).ok()?;
    if tag != 0x30 {
        return None;
    }

    let mut best: Option<EncryptionType> = None;

    let mut pos = 0;
    while pos < seq_data.len() {
        let (entry_tag, entry_data, consumed) = parse_der_tlv(&seq_data[pos..]).ok()?;
        if entry_tag == 0x30 {
            let fields = parse_sequence_fields_local(entry_data).ok()?;
            for (ftag, fvalue) in &fields {
                if *ftag == 0xa0 {
                    if let Ok(etype_val) = parse_der_integer_local(fvalue) {
                        if let Ok(et) = etype_from_i32(etype_val) {
                            match (&best, et) {
                                (None, _) => best = Some(et),
                                (Some(EncryptionType::Rc4Hmac), _)
                                    if et != EncryptionType::Rc4Hmac =>
                                {
                                    best = Some(et);
                                }
                                (
                                    Some(EncryptionType::Aes128CtsHmacSha196),
                                    EncryptionType::Aes256CtsHmacSha196,
                                ) => {
                                    best = Some(et);
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        pos += consumed;
    }

    best
}

// =========================================================================
// Minimal DER helpers (local, to avoid depending on messages.rs internals)
// =========================================================================

/// Parse all TLV elements in a SEQUENCE body.
fn parse_sequence_fields_local(data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, value, consumed) = parse_der_tlv(&data[pos..])?;
        fields.push((tag, value.to_vec()));
        pos += consumed;
    }
    Ok(fields)
}

/// Parse a DER INTEGER TLV, returning i32.
fn parse_der_integer_local(data: &[u8]) -> Result<i32> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != 0x02 {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected INTEGER (0x02), got 0x{tag:02x}"
        )));
    }
    if value.is_empty() {
        return Err(Error::invalid_data("Kerberos: empty INTEGER"));
    }
    let negative = value[0] & 0x80 != 0;
    let mut val: i64 = if negative { -1 } else { 0 };
    for &b in value {
        val = (val << 8) | (b as i64);
    }
    Ok(val as i32)
}

/// Parse a DER OCTET STRING TLV, returning the raw bytes.
fn parse_der_octet_string_local(data: &[u8]) -> Result<Vec<u8>> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != 0x04 {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected OCTET STRING (0x04), got 0x{tag:02x}"
        )));
    }
    Ok(value.to_vec())
}

// =========================================================================
// DER encoding helpers
// =========================================================================

/// Wrap an AP-REQ in a Kerberos GSS-API initial context token (RFC 1964):
/// APPLICATION [0] { OID, 0x0100, AP-REQ }. Windows SPNEGO expects this
/// wrapping in the NegTokenInit mechToken.
fn wrap_gss_ap_req(ap_req: &[u8]) -> Vec<u8> {
    // Standard Kerberos OID 1.2.840.113554.1.2.2 (for GSS inner token)
    let oid_bytes: &[u8] = &OID_KERBEROS[2..]; // skip tag+length
    let mut inner = Vec::new();
    inner.push(0x06); // OID tag
    inner.push(oid_bytes.len() as u8);
    inner.extend_from_slice(oid_bytes);
    inner.extend_from_slice(&[0x01, 0x00]); // KRB_AP_REQ token ID
    inner.extend_from_slice(ap_req);

    der_tlv(0x60, &inner) // APPLICATION [0]
}

/// Encode a context-specific constructed tag.
fn der_context(tag_num: u8, data: &[u8]) -> Vec<u8> {
    der_tlv(0xa0 | tag_num, data)
}

/// Encode an ASN.1 INTEGER.
fn der_integer(val: i32) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    let mut start = 0;
    if val >= 0 {
        while start < 3 && bytes[start] == 0x00 && bytes[start + 1] & 0x80 == 0 {
            start += 1;
        }
    } else {
        while start < 3 && bytes[start] == 0xff && bytes[start + 1] & 0x80 != 0 {
            start += 1;
        }
    }
    der_tlv(0x02, &bytes[start..])
}

/// Encode a DER OCTET STRING.
fn der_octet_string(data: &[u8]) -> Vec<u8> {
    der_tlv(0x04, data)
}

/// Encode a DER SEQUENCE from pre-encoded items.
fn der_sequence(items: &[&[u8]]) -> Vec<u8> {
    let mut contents = Vec::new();
    for item in items {
        contents.extend_from_slice(item);
    }
    der_tlv(0x30, &contents)
}

// =========================================================================
// Time and random helpers
// =========================================================================

/// Get the current time in Kerberos GeneralizedTime format and microseconds.
///
/// Format: "YYYYMMDDHHmmssZ" (UTC).
fn current_kerberos_time() -> (String, u32) {
    use std::time::SystemTime;

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before epoch");

    let total_secs = now.as_secs();
    let usec = now.subsec_micros();

    // Convert seconds since epoch to date/time components.
    // This is a simplified UTC calculation (no leap seconds, which is fine
    // for Kerberos timestamps).
    let (year, month, day, hour, minute, second) = secs_to_datetime(total_secs);

    let time_str = format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}Z",
        year, month, day, hour, minute, second
    );

    (time_str, usec)
}

/// Convert seconds since Unix epoch to (year, month, day, hour, minute, second).
fn secs_to_datetime(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    // Days since epoch.
    let days = secs / 86400;
    let time_of_day = secs % 86400;

    let hour = (time_of_day / 3600) as u32;
    let minute = ((time_of_day % 3600) / 60) as u32;
    let second = (time_of_day % 60) as u32;

    // Civil date from days since 1970-01-01 (algorithm from Howard Hinnant).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    (y as u32, m as u32, d as u32, hour, minute, second)
}

/// Convert an etype integer code to an [`EncryptionType`] enum value.
fn etype_from_code(code: i32) -> Result<EncryptionType> {
    match code {
        18 => Ok(EncryptionType::Aes256CtsHmacSha196),
        17 => Ok(EncryptionType::Aes128CtsHmacSha196),
        23 => Ok(EncryptionType::Rc4Hmac),
        other => Err(Error::Auth {
            message: format!("unsupported etype {other}"),
        }),
    }
}

/// Generate a random 32-bit nonce.
fn generate_nonce() -> u32 {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("CSPRNG failed");
    u32::from_ne_bytes(buf) & 0x7FFF_FFFF // Ensure positive (Kerberos nonce is UInt32 but some KDCs treat it as signed)
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The GSS wrapper is the outermost layer of the AP-REQ, so it is the
    /// first to pass 64 KiB when a large-AD PAC fattens the ticket (#6). Its
    /// length has to describe every byte that follows.
    #[test]
    fn a_gss_token_past_64k_declares_its_full_length() {
        let ap_req = vec![0x6e; 70_000];
        let token = wrap_gss_ap_req(&ap_req);
        let (tag, value, total) = crate::auth::der::parse_der_tlv(&token).unwrap();
        assert_eq!(tag, 0x60);
        assert_eq!(total, token.len());
        assert!(value.ends_with(&ap_req));
    }
    use crate::auth::kerberos::crypto::{
        generate_random_key, kerberos_decrypt, kerberos_encrypt, string_to_key_aes,
    };
    use crate::auth::kerberos::messages::{
        encode_ap_req, encode_as_req, encode_authenticator, encode_pa_enc_timestamp, EncryptedData,
        PrincipalName, Ticket,
    };
    use crate::auth::spnego::OID_NTLMSSP;

    // ── Time formatting tests ────────────────────────────────────────

    #[test]
    fn secs_to_datetime_epoch() {
        let (y, m, d, h, mi, s) = secs_to_datetime(0);
        assert_eq!((y, m, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn secs_to_datetime_known_date() {
        // 2026-04-08 12:00:00 UTC
        // Unix timestamp: 1775649600
        let (y, m, d, h, mi, s) = secs_to_datetime(1775649600);
        assert_eq!((y, m, d, h, mi, s), (2026, 4, 8, 12, 0, 0));
    }

    #[test]
    fn secs_to_datetime_leap_year() {
        // 2024-02-29 00:00:00 UTC
        // Unix timestamp: 1709164800
        let (y, m, d, _, _, _) = secs_to_datetime(1709164800);
        assert_eq!((y, m, d), (2024, 2, 29));
    }

    #[test]
    fn current_kerberos_time_format() {
        let (time_str, _cusec) = current_kerberos_time();
        assert_eq!(
            time_str.len(),
            15,
            "GeneralizedTime should be 15 chars: {time_str}"
        );
        assert!(time_str.ends_with('Z'), "should end with Z: {time_str}");
        // Should be parseable: YYYYMMDDHHMMSSZ
        assert!(time_str[..4].parse::<u32>().is_ok(), "year: {time_str}");
    }

    // ── Nonce generation ─────────────────────────────────────────────

    #[test]
    fn generate_nonce_is_positive() {
        for _ in 0..100 {
            let n = generate_nonce();
            assert!(n <= 0x7FFF_FFFF, "nonce should be positive: {n}");
        }
    }

    #[test]
    fn generate_nonce_not_constant() {
        let n1 = generate_nonce();
        let n2 = generate_nonce();
        // With 31 bits, collision probability is ~2^-31, negligible.
        // But allow it just in case.
        if n1 == n2 {
            let n3 = generate_nonce();
            assert!(
                n1 != n3 || n2 != n3,
                "three consecutive identical nonces is suspicious"
            );
        }
    }

    // ── PA-PAC-REQUEST encoding ──────────────────────────────────────

    #[test]
    fn encode_pa_pac_request_true() {
        let encoded = encode_pa_pac_request(true);
        // SEQUENCE { [0] BOOLEAN TRUE }
        assert_eq!(encoded[0], 0x30); // SEQUENCE
                                      // Should contain 0xff for TRUE
        assert!(encoded.windows(3).any(|w| w == [0x01, 0x01, 0xff]));
    }

    #[test]
    fn encode_pa_pac_request_false() {
        let encoded = encode_pa_pac_request(false);
        assert_eq!(encoded[0], 0x30);
        assert!(encoded.windows(3).any(|w| w == [0x01, 0x01, 0x00]));
    }

    // ── PA-ENC-TIMESTAMP encrypt ─────────────────────────────────────

    #[test]
    fn pa_enc_timestamp_produces_valid_encrypted_data() {
        let key = string_to_key_aes("password", "EXAMPLE.COMuser", 32);
        let timestamp_plain = encode_pa_enc_timestamp("20260408120000Z", 123456);

        let ciphertext = kerberos_encrypt(
            &key,
            KEY_USAGE_PA_ENC_TIMESTAMP,
            &timestamp_plain,
            EncryptionType::Aes256CtsHmacSha196,
        );

        // Should be non-empty and longer than just the HMAC.
        assert!(
            ciphertext.len() > 12,
            "ciphertext too short: {}",
            ciphertext.len()
        );

        // Should decrypt successfully.
        let decrypted = kerberos_decrypt(
            &key,
            KEY_USAGE_PA_ENC_TIMESTAMP,
            &ciphertext,
            EncryptionType::Aes256CtsHmacSha196,
        )
        .unwrap();

        assert_eq!(decrypted, timestamp_plain);
    }

    // ── Authenticator encrypt ────────────────────────────────────────

    #[test]
    fn authenticator_encrypt_decrypt_roundtrip() {
        let key = generate_random_key(EncryptionType::Aes256CtsHmacSha196);

        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let authenticator_plain = encode_authenticator(
            "EXAMPLE.COM",
            &cname,
            "20260408120000Z",
            0,
            None,
            None,
            None,
        );

        let encrypted = kerberos_encrypt(
            &key,
            KEY_USAGE_AP_REQ_AUTHENTICATOR,
            &authenticator_plain,
            EncryptionType::Aes256CtsHmacSha196,
        );

        let decrypted = kerberos_decrypt(
            &key,
            KEY_USAGE_AP_REQ_AUTHENTICATOR,
            &encrypted,
            EncryptionType::Aes256CtsHmacSha196,
        )
        .unwrap();

        assert_eq!(decrypted, authenticator_plain);
    }

    // ── AP-REQ construction ──────────────────────────────────────────

    #[test]
    fn build_ap_req_produces_spnego_wrapped_token() {
        // Build a fake service ticket.
        let ticket = Ticket {
            tkt_vno: 5,
            realm: "EXAMPLE.COM".to_string(),
            sname: PrincipalName {
                name_type: 2,
                name_string: vec!["cifs".to_string(), "server.example.com".to_string()],
            },
            enc_part: EncryptedData {
                etype: 18,
                kvno: Some(1),
                cipher: vec![0xDE, 0xAD, 0xBE, 0xEF],
            },
            raw_bytes: None,
        };

        let session_key = generate_random_key(EncryptionType::Aes256CtsHmacSha196);

        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };

        let authenticator_plain = encode_authenticator(
            "EXAMPLE.COM",
            &cname,
            "20260408120000Z",
            0,
            None,
            None,
            None,
        );

        let encrypted_auth = kerberos_encrypt(
            &session_key,
            KEY_USAGE_AP_REQ_AUTHENTICATOR,
            &authenticator_plain,
            EncryptionType::Aes256CtsHmacSha196,
        );

        let auth_enc_data = EncryptedData {
            etype: 18,
            kvno: None,
            cipher: encrypted_auth,
        };

        let ap_req = encode_ap_req(&ticket, &auth_enc_data, false);

        // AP-REQ should start with APPLICATION [14] = 0x6e.
        assert_eq!(ap_req[0], 0x6e, "AP-REQ should start with APPLICATION [14]");

        // Wrap in SPNEGO.
        let spnego = wrap_neg_token_init(&[OID_KERBEROS, OID_NTLMSSP], &ap_req);

        // SPNEGO NegTokenInit starts with APPLICATION [0] = 0x60.
        assert_eq!(
            spnego[0], 0x60,
            "SPNEGO token should start with APPLICATION [0]"
        );

        // Should contain the SPNEGO OID.
        assert!(
            spnego
                .windows(OID_KERBEROS.len())
                .any(|w| w == OID_KERBEROS),
            "SPNEGO token should contain the Kerberos OID"
        );
    }

    // ── AS-REQ construction ──────────────────────────────────────────

    #[test]
    fn as_req_with_padata_contains_pa_types() {
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let sname = PrincipalName {
            name_type: 2,
            name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
        };

        let pa_pac = PaData {
            padata_type: PA_PAC_REQUEST,
            padata_value: encode_pa_pac_request(true),
        };

        let encoded = encode_as_req(
            &cname,
            "EXAMPLE.COM",
            &sname,
            12345,
            &[EncryptionType::Aes256CtsHmacSha196],
            &[pa_pac],
        );

        // Should start with APPLICATION [10] = 0x6a.
        assert_eq!(encoded[0], 0x6a);

        // Should be non-trivial size (with padata it's bigger).
        assert!(
            encoded.len() > 50,
            "AS-REQ with padata should be substantial"
        );
    }

    // ── EncryptedData encoding ───────────────────────────────────────

    #[test]
    fn encode_encrypted_data_raw_has_sequence_tag() {
        let ed = EncryptedData {
            etype: 18,
            kvno: None,
            cipher: vec![0x01, 0x02, 0x03],
        };
        let encoded = encode_encrypted_data_raw(&ed);
        assert_eq!(encoded[0], 0x30, "EncryptedData should be a SEQUENCE");
    }

    #[test]
    fn encode_encrypted_data_raw_with_kvno() {
        let ed = EncryptedData {
            etype: 18,
            kvno: Some(2),
            cipher: vec![0x01, 0x02, 0x03],
        };
        let encoded = encode_encrypted_data_raw(&ed);
        // Should contain the kvno field (context tag [1]).
        assert!(
            encoded.windows(2).any(|w| w[0] == 0xa1),
            "should contain kvno field [1]"
        );
    }

    // ── ETYPE-INFO2 parsing ──────────────────────────────────────────

    #[test]
    fn parse_etype_info2_best_aes256() {
        // Build a minimal ETYPE-INFO2 with AES-256 and RC4.
        // SEQUENCE { SEQUENCE { [0] INTEGER 18 }, SEQUENCE { [0] INTEGER 23 } }
        let entry_18 = der_sequence(&[&der_context(0, &der_integer(18))]);
        let entry_23 = der_sequence(&[&der_context(0, &der_integer(23))]);
        let etype_info2 = der_sequence(&[&entry_18, &entry_23]);

        let best = parse_etype_info2_best(&etype_info2);
        assert_eq!(best, Some(EncryptionType::Aes256CtsHmacSha196));
    }

    #[test]
    fn parse_etype_info2_best_prefers_aes256_over_aes128() {
        let entry_17 = der_sequence(&[&der_context(0, &der_integer(17))]);
        let entry_18 = der_sequence(&[&der_context(0, &der_integer(18))]);
        let etype_info2 = der_sequence(&[&entry_17, &entry_18]);

        let best = parse_etype_info2_best(&etype_info2);
        assert_eq!(best, Some(EncryptionType::Aes256CtsHmacSha196));
    }

    #[test]
    fn parse_etype_info2_best_rc4_only() {
        let entry_23 = der_sequence(&[&der_context(0, &der_integer(23))]);
        let etype_info2 = der_sequence(&[&entry_23]);

        let best = parse_etype_info2_best(&etype_info2);
        assert_eq!(best, Some(EncryptionType::Rc4Hmac));
    }

    #[test]
    fn parse_etype_info2_best_unknown_only() {
        let entry_99 = der_sequence(&[&der_context(0, &der_integer(99))]);
        let etype_info2 = der_sequence(&[&entry_99]);

        let best = parse_etype_info2_best(&etype_info2);
        assert_eq!(best, None);
    }

    // ── METHOD-DATA parsing ──────────────────────────────────────────

    #[test]
    fn parse_method_data_extracts_padata() {
        // Build METHOD-DATA: SEQUENCE { PA-DATA { type=19, value=<some bytes> } }
        let pa_value = vec![0x01, 0x02, 0x03];
        let pa_type_enc = der_context(1, &der_integer(PA_ETYPE_INFO2));
        let pa_value_enc = der_context(2, &der_octet_string(&pa_value));
        let pa_data = der_sequence(&[&pa_type_enc, &pa_value_enc]);
        let method_data = der_sequence(&[&pa_data]);

        let entries = parse_method_data(&method_data).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].padata_type, PA_ETYPE_INFO2);
        assert_eq!(entries[0].padata_value, pa_value);
    }

    // ── KerberosAuthenticator state ──────────────────────────────────

    #[test]
    fn authenticator_initial_state() {
        let auth = KerberosAuthenticator::new(KerberosCredentials {
            username: "user".to_string(),
            password: "pass".to_string(),
            realm: "EXAMPLE.COM".to_string(),
            kdc_address: "kdc.example.com".to_string(),
        });

        assert!(auth.token().is_none());
        assert!(auth.session_key().is_none());
        assert!(auth.tgt.is_none());
        assert!(auth.as_session_key.is_none());
        assert!(auth.service_ticket.is_none());
        assert!(auth.tgs_session_key.is_none());
    }

    // ── User key derivation ──────────────────────────────────────────

    #[test]
    fn derive_user_key_aes256() {
        let auth = KerberosAuthenticator {
            credentials: KerberosCredentials {
                username: "user".to_string(),
                password: "password".to_string(),
                realm: "EXAMPLE.COM".to_string(),
                kdc_address: "kdc.example.com".to_string(),
            },
            tgt: None,
            as_session_key: None,
            service_ticket: None,
            tgs_session_key: None,
            ap_req_bytes: None,
            session_key: None,
            etype: EncryptionType::Aes256CtsHmacSha196,
        };

        let key = auth.derive_user_key();
        assert_eq!(key.len(), 32, "AES-256 key should be 32 bytes");

        // Should match direct call.
        let expected = string_to_key_aes("password", "EXAMPLE.COMuser", 32);
        assert_eq!(key, expected);
    }

    #[test]
    fn derive_user_key_aes128() {
        let mut auth = KerberosAuthenticator::new(KerberosCredentials {
            username: "user".to_string(),
            password: "password".to_string(),
            realm: "EXAMPLE.COM".to_string(),
            kdc_address: "kdc.example.com".to_string(),
        });
        auth.etype = EncryptionType::Aes128CtsHmacSha196;

        let key = auth.derive_user_key();
        assert_eq!(key.len(), 16, "AES-128 key should be 16 bytes");
    }

    #[test]
    fn derive_user_key_rc4() {
        let mut auth = KerberosAuthenticator::new(KerberosCredentials {
            username: "user".to_string(),
            password: "password".to_string(),
            realm: "EXAMPLE.COM".to_string(),
            kdc_address: "kdc.example.com".to_string(),
        });
        auth.etype = EncryptionType::Rc4Hmac;

        let key = auth.derive_user_key();
        assert_eq!(key.len(), 16, "RC4 key (NT hash) should be 16 bytes");
    }

    #[test]
    fn kerberos_credentials_debug_output_hides_the_password() {
        let creds = KerberosCredentials {
            username: "smbtest".to_string(),
            password: "hunter2-secret".to_string(),
            realm: "TEST.LOCAL".to_string(),
            kdc_address: "10.0.0.1".to_string(),
        };
        let printed = format!("{creds:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("password: ***"), "{printed}");
        assert!(printed.contains("TEST.LOCAL"), "{printed}");
    }
}
