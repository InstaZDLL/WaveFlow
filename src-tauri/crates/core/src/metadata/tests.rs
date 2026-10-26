use super::test_server::{serve, serve_json, Reply};
use super::{default_http, require_secure, BaseUrlError};

/// The desktop's client follows a redirect only inside the origin it
/// asked: another port is another origin, so the target is never
/// contacted and the redirect comes back as the answer.
#[tokio::test]
async fn the_desktop_client_does_not_follow_a_redirect_out_of_its_origin() {
    let elsewhere = serve_json("{}").await;
    let location = format!("location: {}/", elsewhere.base_url);
    let asked = serve(Reply {
        status: "307 Temporary Redirect",
        headers: &[&location],
        body: "",
        announce_length: true,
    })
    .await;

    let response = default_http("test", 5)
        .post(format!("{}/", asked.base_url))
        .form(&[("sk", "a-session-key")])
        .send()
        .await
        .unwrap();

    assert_eq!(response.status().as_u16(), 307);
    assert_eq!(elsewhere.hits(), 0, "the signed form was sent elsewhere");
}

fn verdict(url: &str) -> Result<(), String> {
    require_secure(url.to_owned())
        .map(|_| ())
        .map_err(|err| match err {
            BaseUrlError::Invalid => "invalid".to_owned(),
            BaseUrlError::Plaintext { host } => format!("plaintext to {host}"),
        })
}

#[test]
fn https_is_accepted_anywhere() {
    assert_eq!(verdict("https://ws.audioscrobbler.com/2.0/"), Ok(()));
}

#[test]
fn plaintext_is_accepted_to_this_machine_only() {
    assert_eq!(verdict("http://127.0.0.1:8080/2.0/"), Ok(()));
    assert_eq!(verdict("http://[::1]:8080/2.0/"), Ok(()));
    assert_eq!(verdict("http://localhost/2.0/"), Ok(()));
    assert_eq!(
        verdict("http://ws.audioscrobbler.com/2.0/"),
        Err("plaintext to ws.audioscrobbler.com".to_owned())
    );
}

/// A name that merely starts like a loopback address is a remote host.
#[test]
fn a_host_that_only_looks_local_is_remote() {
    assert_eq!(
        verdict("http://127.0.0.1.attacker.example/"),
        Err("plaintext to 127.0.0.1.attacker.example".to_owned())
    );
    assert_eq!(
        verdict("http://localhost.attacker.example/"),
        Err("plaintext to localhost.attacker.example".to_owned())
    );
}

#[test]
fn another_scheme_or_no_url_is_refused() {
    assert!(verdict("ftp://127.0.0.1/").is_err());
    assert_eq!(
        verdict("ws.audioscrobbler.com/2.0/"),
        Err("invalid".to_owned())
    );
}
