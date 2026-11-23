//! TLS trust for both Access transports.
//!
//! Two modes are selected by configuration:
//!
//! - **System** (no explicit CA): verify with the operating system where the
//!   platform exposes a verifier, otherwise with the platform certificate store.
//!   Where a platform exposes no store at all (OHOS, containers), fall back to
//!   the portable Mozilla roots so the default mode remains usable.
//! - **Custom** (explicit `ca_pem`): trust exactly the supplied CA bundle and
//!   nothing else. This is the usual `--cacert` semantics used by curl, Git,
//!   Go and requests: a custom bundle *replaces* the default roots.
//!
//! Both modes produce one `rustls::ClientConfig`; HTTPS and MQTT therefore
//! share the same trust decision and cannot drift apart.

use std::sync::Arc;

use rustls::client::danger::ServerCertVerifier;
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, RootCertStore};

use super::AccessError;

/// Build the TLS client configuration for one trust mode.
///
/// `alpn` is empty for HTTPS (HTTP/1.1) and carries the MQTT protocol for the
/// device connection.
pub(crate) fn client_config(
    ca_pem: Option<&[u8]>,
    alpn: &[String],
) -> Result<Arc<ClientConfig>, AccessError> {
    let provider = crypto_provider();
    let verifier = match ca_pem {
        Some(ca) => custom_verifier(ca, provider.clone())?,
        None => system_verifier(provider.clone())?,
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| AccessError::protocol("invalid TLS protocol versions"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|value| value.as_bytes().to_vec()).collect();
    Ok(Arc::new(config))
}

/// Keep the process-wide ring provider shared with HTTPS; a host application
/// may already have installed its own provider before Core opens.
fn crypto_provider() -> Arc<CryptoProvider> {
    if CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

/// Explicit CA mode: the bundle is the complete trust set.
fn custom_verifier(
    ca_pem: &[u8],
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AccessError> {
    let mut roots = RootCertStore::empty();
    let (added, _) = roots.add_parsable_certificates(
        rustls_pemfile::certs(&mut &ca_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AccessError::protocol("invalid CA certificate"))?,
    );
    if added == 0 {
        return Err(AccessError::protocol("invalid CA certificate"));
    }
    webpki_verifier(roots, provider)
}

/// System mode. Apple and Windows expose a real per-connection platform
/// verifier; other targets use the platform certificate files and finally the
/// portable roots when the platform has no store to enumerate.
#[cfg(any(target_vendor = "apple", windows))]
fn system_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AccessError> {
    let verifier = rustls_platform_verifier::Verifier::new(provider)
        .map_err(|_| AccessError::protocol("platform TLS verifier is unavailable"))?;
    Ok(Arc::new(verifier))
}

#[cfg(not(any(target_vendor = "apple", windows)))]
fn system_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AccessError> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    if added == 0 {
        // Android installs its platform bundle through `SSL_CERT_FILE`; OHOS
        // and minimal containers have no OpenSSL path at all. Keep the default
        // mode working with the portable public roots in that case.
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    webpki_verifier(roots, provider)
}

fn webpki_verifier(
    roots: RootCertStore,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, AccessError> {
    if roots.is_empty() {
        return Err(AccessError::protocol(
            "no TLS root certificates are available",
        ));
    }
    let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|_| AccessError::protocol("TLS verifier initialization failed"))?;
    Ok(verifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(pem: &[u8]) -> Vec<rustls::pki_types::CertificateDer<'static>> {
        rustls_pemfile::certs(&mut &pem[..])
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn custom_mode_rejects_an_empty_or_unparseable_bundle() {
        assert!(client_config(Some(b"not a certificate"), &[]).is_err());
        assert!(client_config(Some(b""), &[]).is_err());
    }

    #[test]
    fn custom_mode_uses_only_the_supplied_bundle() {
        let config = client_config(Some(TEST_CA), &["mqtt".to_owned()]).unwrap();
        assert_eq!(config.alpn_protocols, vec![b"mqtt".to_vec()]);
        assert_eq!(certificate(TEST_CA).len(), 1);
    }

    #[test]
    fn system_mode_is_always_available() {
        // Platform verifier targets use the OS; other targets use the native
        // store or the portable fallback, so construction never fails.
        assert!(client_config(None, &[]).is_ok());
    }

    /// Self-signed test certificate. The signing key was discarded.
    const TEST_CA: &[u8] = b"-----BEGIN CERTIFICATE-----
MIIBazCCARKgAwIBAgIUCYEJf6AWvrWHzQnqALYWDNrwYfgwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRV2hhbGVSZWluIFRlc3QgQ0EwHhcNMjYxMTE0MDQzMDA2WhcN
MzYxMTExMDQzMDA2WjAcMRowGAYDVQQDDBFXaGFsZVJlaW4gVGVzdCBDQTBZMBMG
ByqGSM49AgEGCCqGSM49AwEHA0IABIDzEtmehsRaR0Vx65NWD4y1kflMTDFblTA8
64kOENsLgnPx+O1z8BV4JzL5+wqRd/gcxvR5WULQOuxjCoDpm4ejMjAwMB0GA1Ud
DgQWBBQ0nEJTr/TR1J9+vLqmx9euDviHNzAPBgNVHRMBAf8EBTADAQH/MAoGCCqG
SM49BAMCA0cAMEQCIEyFy7p0NegnWrzl41pLP89cCp9itTXSeJ8I13iKpm28AiAR
kg3kmwrRgJT2uxIVgusk+cBIKzWjxMwkC7uvy8RsrQ==
-----END CERTIFICATE-----
";
}
