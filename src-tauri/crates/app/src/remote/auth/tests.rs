use super::*;

#[test]
fn the_challenge_is_what_the_server_will_accept() {
    // The server validates canonically: exactly 43 characters of
    // base64url decoding to a 32-byte digest. Anything else is
    // rejected before a grant is issued.
    let verifier = random_token();
    let challenge = pkce_challenge(&verifier);
    assert_eq!(challenge.len(), 43);
    assert_eq!(URL_SAFE_NO_PAD.decode(&challenge).unwrap().len(), 32);
    assert!(!challenge.contains('+') && !challenge.contains('/') && !challenge.contains('='));
}

#[test]
fn the_verifier_sits_inside_the_length_the_server_allows() {
    let verifier = random_token();
    assert_eq!(verifier.len(), 64);
    assert!((43..=128).contains(&verifier.len()));
}

#[test]
fn each_handshake_draws_fresh_secrets() {
    assert_ne!(random_token(), random_token());
}

#[test]
fn a_known_verifier_hashes_to_its_known_challenge() {
    // Pinned against the RFC 7636 appendix B vector, so a future
    // change of encoder or digest cannot pass silently.
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn the_device_name_never_comes_back_empty() {
    // It is sent on every sign-in and the server rejects an empty
    // one, so the fallback matters more than the hostname does.
    let name = device_name();
    assert!(!name.trim().is_empty());
    assert!(name.len() <= MAX_DEVICE_NAME);
}

#[test]
fn a_long_hostname_is_trimmed_rather_than_failing_the_sign_in() {
    let name = truncate_device_name(format!("WaveFlow on {}", "h".repeat(400)));
    assert_eq!(name.len(), MAX_DEVICE_NAME);
}

#[test]
fn truncation_lands_on_a_character_boundary() {
    // The server counts bytes; a hostname may not be ASCII. Cutting
    // mid-character would produce invalid UTF-8 and panic on
    // `truncate`.
    let name = truncate_device_name(format!("WaveFlow on {}", "é".repeat(200)));
    assert!(name.len() <= MAX_DEVICE_NAME);
    assert!(name.chars().next_back().is_some());
}

#[test]
fn a_short_name_is_left_alone() {
    assert_eq!(
        truncate_device_name("WaveFlow on nas".into()),
        "WaveFlow on nas"
    );
}
