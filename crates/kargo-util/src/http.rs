//! The HTTP client settings shared by every download: the proxy (`[http] proxy`, else the
//! `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY` environment variables) and the trusted
//! certificate authorities (`[http] cainfo`, else the built-in Mozilla roots).

use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme, StreamOwned};
use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig};

use crate::config::Config;

/// An agent with Kargo's user agent and network settings, that reports HTTP errors as responses.
pub fn agent(config: &Config, recv_body_timeout: Duration) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(recv_body_timeout))
        .http_status_as_error(false)
        .user_agent(concat!("kargo/", env!("CARGO_PKG_VERSION")));
    let net = config.network();
    if let Some(proxy) = &net.proxy {
        builder = builder.proxy(proxy.clone());
    }
    if let Some(certs) = &net.root_certs {
        builder = builder.tls_config(TlsConfig::builder().root_certs(RootCerts::new_with_certs(certs)).build());
    }
    builder.build().into()
}

/// The proxy and certificates of the settings, checked once when the configuration is loaded.
#[derive(Debug, Clone, Default)]
pub struct Network {
    /// `None` keeps ureq's default: the proxy of the environment.
    proxy: Option<Option<ureq::Proxy>>,
    root_certs: Option<Vec<Certificate<'static>>>,
}

impl Network {
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self> {
        let proxy = match settings.proxy() {
            None => None,
            Some("") => Some(None),
            Some(url) => Some(Some(ureq::Proxy::new(url).with_context(|| format!("invalid [http] `proxy` `{url}`"))?)),
        };
        let root_certs = match settings.cainfo() {
            None => None,
            Some(path) => {
                let pem = std::fs::read(path).with_context(|| format!("failed to read the [http] `cainfo` bundle {}", path.display()))?;
                let mut certs = Vec::new();
                for item in ureq::tls::parse_pem(&pem) {
                    if let PemItem::Certificate(cert) = item.with_context(|| format!("invalid PEM in {}", path.display()))? {
                        certs.push(cert);
                    }
                }
                if certs.is_empty() {
                    bail!("the [http] `cainfo` bundle {} holds no PEM certificates", path.display());
                }
                Some(certs)
            }
        };
        Ok(Self { proxy, root_certs })
    }
}

impl Network {
    /// The DER of the certificate authorities of `[http] cainfo`, if there are any.
    fn root_cert_ders(&self) -> Option<Vec<Vec<u8>>> {
        self.root_certs.as_ref().map(|certs| certs.iter().map(|c| c.der().to_vec()).collect())
    }
}

/// A TLS connection to a server, for code that speaks HTTP over a socket itself (the dev server's proxy).
pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// Starts TLS over `tcp` to the server called `host`, trusting `[http] cainfo`, else the built-in Mozilla roots. With
/// `verify` off any certificate is accepted, which is for a development server with a self-signed one. The handshake
/// happens on the first read or write.
pub fn tls_connect(network: &Network, host: &str, tcp: TcpStream, verify: bool) -> Result<TlsStream> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder =
        ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions().context("failed to set up TLS")?;
    let client = if verify {
        let mut roots = RootCertStore::empty();
        match network.root_cert_ders() {
            Some(ders) => {
                for der in ders {
                    roots.add(CertificateDer::from(der)).context("a certificate of the [http] `cainfo` bundle is not valid")?;
                }
            }
            None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertificate(provider.signature_verification_algorithms)))
            .with_no_client_auth()
    };
    let name = ServerName::try_from(host.to_owned()).with_context(|| format!("`{host}` is not a valid host name"))?;
    let connection = ClientConnection::new(Arc::new(client), name).context("failed to start TLS")?;
    Ok(StreamOwned::new(connection, tcp))
}

/// Trusts every server certificate. Only for the target of a proxy route that says `insecure = true`.
#[derive(Debug)]
struct AcceptAnyCertificate(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

/// The `Authorization` header value for HTTP basic authentication.
pub fn basic_auth(username: &str, password: &str) -> String {
    use base64::Engine as _;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use std::path::Path;

    #[test]
    fn rejects_bundles_without_certificates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.pem"), "not a certificate\n").unwrap();
        let mut settings = Settings::default();
        settings.merge_str("[http]\ncainfo = \"ca.pem\"\n", dir.path()).unwrap();
        let err = Network::from_settings(&settings).unwrap_err().to_string();
        assert!(err.contains("no PEM certificates"), "{err}");

        let mut settings = Settings::default();
        settings.merge_str("[http]\ncainfo = \"missing.pem\"\n", Path::new(dir.path())).unwrap();
        assert!(Network::from_settings(&settings).is_err());
    }

    #[test]
    fn an_empty_proxy_turns_the_environment_off() {
        let mut settings = Settings::default();
        settings.merge_str("[http]\nproxy = \"\"\n", Path::new("/")).unwrap();
        assert!(matches!(Network::from_settings(&settings).unwrap().proxy, Some(None)));
        assert!(Network::from_settings(&Settings::default()).unwrap().proxy.is_none());
    }
}
