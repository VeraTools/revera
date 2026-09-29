//! HTTPS trust roots shared by every outbound client.

/// Adds the bundled Mozilla roots on top of the platform store. The
/// release is a static musl binary, and on a host with no system CA bundle
/// (minimal containers) reqwest's platform verifier refuses to build a
/// client unless extra roots are supplied; system and enterprise CAs still
/// apply.
pub fn with_bundled_roots(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder.tls_certs_merge(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|c| reqwest::Certificate::from_der(c.as_ref()).ok()),
    )
}
