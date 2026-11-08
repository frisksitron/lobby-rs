//! DTLS implementation using dimpl with RustCrypto backend.
//!
//! lobby-rs patch: the certificate is generated here with RustCrypto instead of dimpl's rcgen
//! helper (which needs aws-lc), and the dimpl config names the RustCrypto provider.

use std::sync::Arc;
use std::time::Instant;

use str0m_proto::crypto::CryptoError;
use str0m_proto::crypto::DtlsVersion;
use str0m_proto::crypto::dtls::ProtocolVersion;
use str0m_proto::crypto::dtls::{DtlsCert, DtlsImplError, DtlsInstance, DtlsOutput, DtlsProvider};

// ============================================================================
// DTLS Provider Implementation
// ============================================================================

#[derive(Debug)]
pub(super) struct RustCryptoDtlsProvider;

impl DtlsProvider for RustCryptoDtlsProvider {
    fn generate_certificate(&self) -> Option<DtlsCert> {
        match self_signed_p256() {
            Ok(cert) => Some(cert),
            Err(e) => {
                // str0m turns `None` into its own error; say why here.
                eprintln!("str0m-rust-crypto: certificate generation failed: {e}");
                None
            }
        }
    }

    fn new_dtls(
        &self,
        cert: &DtlsCert,
        now: Instant,
        dtls_version: DtlsVersion,
        mtu: Option<usize>,
    ) -> Result<Box<dyn DtlsInstance>, CryptoError> {
        let dimpl_cert = dimpl::DtlsCertificate {
            certificate: cert.certificate.clone(),
            private_key: cert.private_key.clone(),
        };

        // Create a default dimpl Config with RustCrypto crypto provider
        // ICE verifies return routability before DTLS, making server cookies redundant.
        let mut builder = dimpl::Config::builder()
            .with_crypto_provider(dimpl::crypto::rust_crypto::default_provider())
            .use_server_cookie(false);
        if let Some(mtu) = mtu {
            builder = builder.mtu(mtu);
        }
        if self.is_test() {
            // We need the DTLS impl to be deterministic for the BWE tests.
            builder = builder.dangerously_set_rng_seed(42);
        }

        let config = builder
            .build()
            .map_err(|e| CryptoError::Other(format!("dimpl config creation failed: {}", e)))?;

        let config = Arc::new(config);
        let dtls = match dtls_version {
            DtlsVersion::Dtls12 => dimpl::Dtls::new_12(config, dimpl_cert, now),
            DtlsVersion::Dtls13 => dimpl::Dtls::new_13(config, dimpl_cert, now),
            DtlsVersion::Auto => dimpl::Dtls::new_auto(config, dimpl_cert, now),
            _ => {
                return Err(CryptoError::Other(format!(
                    "Unsupported DTLS version: {dtls_version}"
                )));
            }
        };

        Ok(Box::new(RustCryptoDtlsInstance { dtls }))
    }
}

/// A self-signed ECDSA P-256 certificate for DTLS, like dimpl's rcgen helper makes: subject and
/// issuer `O=DTLS, CN=DTLS Peer`, valid for a year, a random 128-bit serial (Firefox requires
/// serials to be unique across processes: str0m issue 517). The peer checks it only against the
/// SDP fingerprint and the handshake signature.
fn self_signed_p256() -> Result<DtlsCert, String> {
    use std::str::FromStr as _;

    use p256::ecdsa::{DerSignature, SigningKey, signature::Signer as _};
    use p256::pkcs8::{EncodePrivateKey as _, EncodePublicKey as _};
    use rand_core::{OsRng, RngCore as _};
    use x509_cert::der::{Decode as _, Encode as _, asn1::BitString, oid::ObjectIdentifier};
    use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};
    use x509_cert::{Certificate, TbsCertificate, Version, name::Name, serial_number::SerialNumber, time::Validity};

    const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
    const ONE_YEAR: std::time::Duration = std::time::Duration::from_secs(365 * 24 * 3600);

    let key = SigningKey::random(&mut OsRng);
    let spki_der = key
        .verifying_key()
        .to_public_key_der()
        .map_err(|e| e.to_string())?;
    let spki = SubjectPublicKeyInfoOwned::from_der(spki_der.as_bytes()).map_err(|e| e.to_string())?;
    let mut serial = [0u8; 16];
    OsRng.fill_bytes(&mut serial);
    // DER INTEGERs are signed: keep it positive and non-zero.
    serial[0] = (serial[0] & 0x7f) | 0x01;
    let name = Name::from_str("O=DTLS,CN=DTLS Peer").map_err(|e| e.to_string())?;
    let algorithm = AlgorithmIdentifierOwned {
        oid: ECDSA_WITH_SHA256,
        parameters: None,
    };
    let tbs = TbsCertificate {
        version: Version::V3,
        serial_number: SerialNumber::new(&serial).map_err(|e| e.to_string())?,
        signature: algorithm.clone(),
        issuer: name.clone(),
        validity: Validity::from_now(ONE_YEAR).map_err(|e| e.to_string())?,
        subject: name,
        subject_public_key_info: spki,
        issuer_unique_id: None,
        subject_unique_id: None,
        extensions: None,
    };
    let tbs_der = tbs.to_der().map_err(|e| e.to_string())?;
    let signature: DerSignature = key.sign(&tbs_der);
    let cert = Certificate {
        tbs_certificate: tbs,
        signature_algorithm: algorithm,
        signature: BitString::from_bytes(signature.as_bytes()).map_err(|e| e.to_string())?,
    };
    Ok(DtlsCert {
        certificate: cert.to_der().map_err(|e| e.to_string())?,
        private_key: key
            .to_pkcs8_der()
            .map_err(|e| e.to_string())?
            .as_bytes()
            .to_vec(),
    })
}

// ============================================================================
// DTLS Instance Wrapper
// ============================================================================

struct RustCryptoDtlsInstance {
    dtls: dimpl::Dtls,
}

impl std::fmt::Debug for RustCryptoDtlsInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RustCryptoDtlsInstance").finish()
    }
}

impl DtlsInstance for RustCryptoDtlsInstance {
    fn set_active(&mut self, active: bool) {
        self.dtls.set_active(active);
    }

    fn handle_packet(&mut self, packet: &[u8]) -> Result<(), DtlsImplError> {
        self.dtls.handle_packet(packet)
    }

    fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> DtlsOutput<'a> {
        self.dtls.poll_output(buf)
    }

    fn handle_timeout(&mut self, now: Instant) -> Result<(), DtlsImplError> {
        self.dtls.handle_timeout(now)
    }

    fn send_application_data(&mut self, data: &[u8]) -> Result<(), DtlsImplError> {
        self.dtls.send_application_data(data)
    }

    fn is_active(&self) -> bool {
        self.dtls.is_active()
    }

    fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.dtls.protocol_version()
    }

    fn is_closing(&self) -> bool {
        self.dtls.is_closing()
    }

    fn is_closed(&self) -> bool {
        self.dtls.is_closed()
    }

    fn close(&mut self) -> Result<(), DtlsImplError> {
        self.dtls.close()
    }
}
