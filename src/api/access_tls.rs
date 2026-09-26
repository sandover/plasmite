use super::{certificate_validity, spki_digest};
use std::sync::Arc;
use ureq::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use ureq::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use ureq::rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};

pub(super) fn agent(expected_spki: Option<[u8; 32]>) -> ureq::Agent {
    let provider = ureq::rustls::crypto::ring::default_provider();
    let _ = ureq::rustls::crypto::ring::default_provider().install_default();
    let verifier = PinnedServerCertVerifier {
        expected_spki,
        supported: provider.signature_verification_algorithms,
    };
    let tls_config = ureq::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    ureq::builder()
        .tls_config(Arc::new(tls_config))
        .redirects(0)
        .build()
}

#[derive(Debug)]
struct PinnedServerCertVerifier {
    expected_spki: Option<[u8; 32]>,
    supported: ureq::rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let parsed = ureq::rustls::server::ParsedCertificate::try_from(end_entity)?;
        ureq::rustls::client::verify_server_name(&parsed, server_name)?;

        let (not_before, not_after) = certificate_validity(end_entity.as_ref()).map_err(|_| {
            TlsError::InvalidCertificate(ureq::rustls::CertificateError::BadEncoding)
        })?;
        let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        if now < not_before {
            return Err(TlsError::InvalidCertificate(
                ureq::rustls::CertificateError::NotValidYet,
            ));
        }
        if now > not_after {
            return Err(TlsError::InvalidCertificate(
                ureq::rustls::CertificateError::Expired,
            ));
        }

        if let Some(expected) = self.expected_spki {
            let actual = spki_digest(end_entity.as_ref()).map_err(|_| {
                TlsError::InvalidCertificate(ureq::rustls::CertificateError::BadEncoding)
            })?;
            if actual != expected {
                return Err(TlsError::InvalidCertificate(
                    ureq::rustls::CertificateError::UnknownIssuer,
                ));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        ureq::rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.supported,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        ureq::rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.supported,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported.supported_schemes()
    }
}
