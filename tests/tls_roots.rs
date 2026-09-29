//! Network test run by CI's release-build job with `SSL_CERT_FILE` and
//! `SSL_CERT_DIR` pointing at an empty file and directory, i.e. a host
//! without a CA bundle. (Missing paths do not work: cargo replaces them
//! with the probed system bundle before starting the test.)

#[tokio::test]
#[ignore = "network; run with --ignored and an empty system CA bundle"]
async fn https_works_without_system_roots() {
    assert!(
        reqwest::Client::builder().build().is_err(),
        "system roots are still visible; empty them to make this test meaningful"
    );
    let client = revera::tls::with_bundled_roots(reqwest::Client::builder())
        .user_agent("revera-tls-test")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("client builds with bundled roots");
    // any HTTP response proves the handshake; the status (e.g. a 403 rate
    // limit on shared runner IPs) is irrelevant
    client
        .get("https://api.github.com/")
        .send()
        .await
        .expect("TLS handshake with bundled roots");
}
