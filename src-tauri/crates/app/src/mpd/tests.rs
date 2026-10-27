use super::*;

#[tokio::test]
async fn bind_scan_falls_forward_when_the_port_is_taken() {
    // Occupy a port, then ask the scanner for it: it must land on a
    // later one rather than failing outright.
    let squatter = tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
    let taken = squatter.local_addr().unwrap().port();

    let listener = bind_with_scan(taken)
        .await
        .expect("should find a free port");
    let got = listener.local_addr().unwrap().port();
    assert_ne!(got, taken);
    assert!(got > taken && got < taken + config::PORT_SCAN_LEN);
}
