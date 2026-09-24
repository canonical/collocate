use collocate_core::{Error, Result};
use collocate_trust::{fingerprint_der, Identity};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::sync::Arc;

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_error(e: impl std::fmt::Display) -> Error {
    Error::Internal(format!("tls: {e}"))
}

fn chain(pem: &str) -> Result<Vec<CertificateDer<'static>>> {
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<std::result::Result<_, _>>().map_err(tls_error)?;
    if certs.is_empty() {
        return Err(Error::Invalid("no certificate in PEM data".into()));
    }
    Ok(certs)
}

fn key(pem: &str) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).map_err(tls_error)
}

#[derive(Debug)]
struct AnyClientCertificate {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for AnyClientCertificate {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub fn server_config(identity: &Identity) -> Result<Arc<ServerConfig>> {
    let p = provider();
    let config = ServerConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .with_client_cert_verifier(Arc::new(AnyClientCertificate { provider: p }))
        .with_single_cert(chain(&identity.certificate)?, key(&identity.key)?)
        .map_err(tls_error)?;
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct PinnedServer {
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let seen = fingerprint_der(end_entity.as_ref());
        if collocate_trust::constant_time_eq(&seen, &self.fingerprint) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!("server certificate fingerprint {seen} does not match the expected {}", self.fingerprint)))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub fn client_config(identity: &Identity, server_fingerprint: &str) -> Result<Arc<ClientConfig>> {
    let p = provider();
    let config = ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer { fingerprint: server_fingerprint.to_ascii_lowercase(), provider: p }))
        .with_client_auth_cert(chain(&identity.certificate)?, key(&identity.key)?)
        .map_err(tls_error)?;
    Ok(Arc::new(config))
}

pub fn server_name(address: &str) -> Result<ServerName<'static>> {
    let host = match address.rsplit_once(':') {
        Some((h, _)) => h.trim_start_matches('[').trim_end_matches(']'),
        None => address,
    };
    ServerName::try_from(host.to_string()).map_err(|e| Error::Invalid(format!("address {address}: {e}")))
}

pub fn peer_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Option<String> {
    certs.and_then(|c| c.first()).map(|c| fingerprint_der(c.as_ref()))
}

pub fn peer_pem(certs: Option<&[CertificateDer<'_>]>) -> Option<String> {
    certs.and_then(|c| c.first()).map(|c| collocate_trust::der_to_pem(c.as_ref()))
}
