pub mod discovery;
pub(crate) mod signaling;

use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{
    CertificateError, DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::DEFAULT_PORT;

const BASE62_ALPHABET: &[u8; 62] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE62_CODE_LEN: usize = 76;

pub struct Pairing {
    fingerprint: [u8; 32],
    secret: [u8; 24],
}

impl Pairing {
    pub fn parse(code: &str) -> Result<Self> {
        let trimmed = code.trim();
        let bytes: [u8; 56] = if trimmed.len() == BASE62_CODE_LEN {
            decode_base62_56(trimmed)?
        } else if trimmed.len() == 75 {
            URL_SAFE_NO_PAD
                .decode(trimmed)
                .map_err(|_| anyhow!("Invalid pairing code."))?
                .try_into()
                .map_err(|_| anyhow!("Invalid pairing code."))?
        } else {
            return Err(anyhow!("Invalid pairing code."));
        };
        let (fingerprint, secret) = bytes.split_at(32);
        Ok(Self {
            fingerprint: fingerprint.try_into()?,
            secret: secret.try_into()?,
        })
    }

    pub(crate) fn code(&self) -> String {
        let mut bytes = [0; 56];
        bytes[..32].copy_from_slice(&self.fingerprint);
        bytes[32..].copy_from_slice(&self.secret);
        encode_base62_56(&bytes)
    }

    pub(crate) fn identity() -> Result<(Self, CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["lan-remote.local".into()])?;
        let mut secret = [0; 24];
        getrandom::fill(&mut secret)
            .map_err(|error| anyhow!("Pairing randomness unavailable: {error}"))?;
        let certificate = cert.der().clone();
        let pairing = Self {
            fingerprint: Sha256::digest(certificate.as_ref()).into(),
            secret,
        };
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
        Ok((pairing, certificate, key))
    }

    pub(crate) fn verifier(&self, provider: Arc<CryptoProvider>) -> Arc<dyn ServerCertVerifier> {
        Arc::new(PinVerifier {
            fingerprint: self.fingerprint,
            provider,
        })
    }

    pub(crate) fn secret(&self) -> &[u8; 24] {
        &self.secret
    }
}

pub fn endpoint(input: &str) -> Result<SocketAddrV4> {
    let input = input.trim();
    let address = match input.parse::<SocketAddrV4>() {
        Ok(address) => address,
        Err(_) => SocketAddrV4::new(
            input
                .parse()
                .context("Enter a private IPv4 address, optionally followed by a port.")?,
            DEFAULT_PORT,
        ),
    };
    require_allowed(address)?;
    Ok(address)
}

pub fn is_local(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}

pub fn is_cgnat(ip: Ipv4Addr) -> bool {
    matches!(ip.octets(), [100, 64..=127, _, _])
}

pub fn is_allowed_ip(ip: Ipv4Addr) -> bool {
    is_local(ip) || is_cgnat(ip)
}

pub(crate) fn require_allowed(address: SocketAddrV4) -> Result<()> {
    ensure!(
        is_allowed_ip(*address.ip()),
        "Use a private, shared VPN (100.64.0.0/10), link-local, or loopback IPv4 address."
    );
    ensure!(address.port() != 0, "Enter a port from 1 to 65535.");
    Ok(())
}

pub(crate) fn is_allowed_peer(address: SocketAddr) -> bool {
    matches!(address, SocketAddr::V4(address) if is_allowed_ip(*address.ip()))
}

/// Finds the local address selected by the OS for an IPv4 route without sending a packet.
pub(crate) fn local_route(address: SocketAddrV4) -> Result<Ipv4Addr> {
    require_allowed(address)?;
    let route = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    route.connect(address)?;
    let SocketAddr::V4(local) = route.local_addr()? else {
        anyhow::bail!("Expected an IPv4 route.");
    };
    Ok(*local.ip())
}

#[derive(Debug)]
struct PinVerifier {
    fingerprint: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint = Sha256::digest(leaf.as_ref());
        if bool::from(fingerprint.as_slice().ct_eq(&self.fingerprint)) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn encode_base62_56(bytes: &[u8; 56]) -> String {
    let mut work = *bytes;
    let mut encoded = [b'0'; BASE62_CODE_LEN];
    for slot in encoded.iter_mut().rev() {
        let mut rem: u16 = 0;
        for byte in &mut work {
            let acc = (rem << 8) | u16::from(*byte);
            *byte = (acc / 62) as u8;
            rem = acc % 62;
        }
        *slot = BASE62_ALPHABET[usize::from(rem)];
    }
    debug_assert_eq!(work, [0u8; 56]);
    String::from_utf8(encoded.to_vec()).expect("BASE62_ALPHABET is ASCII")
}

fn decode_base62_56(input: &str) -> Result<[u8; 56]> {
    if input.len() != BASE62_CODE_LEN {
        return Err(anyhow!("Invalid pairing code."));
    }
    let mut out = [0u8; 56];
    for &ch in input.as_bytes() {
        let digit = match ch {
            b'0'..=b'9' => ch - b'0',
            b'A'..=b'Z' => ch - b'A' + 10,
            b'a'..=b'z' => ch - b'a' + 36,
            _ => return Err(anyhow!("Invalid pairing code.")),
        };
        let mut carry = u32::from(digit);
        for byte in out.iter_mut().rev() {
            let acc = u32::from(*byte) * 62 + carry;
            *byte = (acc & 0xff) as u8;
            carry = acc >> 8;
        }
        if carry != 0 {
            return Err(anyhow!("Invalid pairing code."));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::{
        BASE62_CODE_LEN, Pairing, decode_base62_56, encode_base62_56, endpoint, is_allowed_ip,
        is_cgnat, is_local,
    };
    use crate::DEFAULT_PORT;

    #[test]
    fn pairing_codes_are_alphanumeric_base62_and_round_trip() {
        for raw in [[0u8; 56], [0xffu8; 56]] {
            let encoded = encode_base62_56(&raw);
            assert_eq!(encoded.len(), BASE62_CODE_LEN);
            assert!(encoded.bytes().all(|b| b.is_ascii_alphanumeric()));
            assert_eq!(decode_base62_56(&encoded).unwrap(), raw);
            let parsed = Pairing::parse(&format!("  {encoded}  ")).unwrap();
            assert_eq!(parsed.code(), encoded);
        }

        let (generated, _, _) = Pairing::identity().unwrap();
        let code = generated.code();
        assert_eq!(code.len(), BASE62_CODE_LEN);
        assert!(code.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert!(!code.contains('-') && !code.contains('_'));
        let parsed = Pairing::parse(&code).unwrap();
        assert_eq!(parsed.fingerprint, generated.fingerprint);
        assert_eq!(parsed.secret, generated.secret);

        let legacy_raw = [0xABu8; 56];
        let legacy_code = URL_SAFE_NO_PAD.encode(legacy_raw);
        let parsed_legacy = Pairing::parse(&legacy_code).unwrap();
        assert_eq!(parsed_legacy.fingerprint, [0xABu8; 32]);
        assert_eq!(parsed_legacy.secret, [0xABu8; 24]);

        assert!(Pairing::parse("").is_err());
        assert!(Pairing::parse(&"z".repeat(BASE62_CODE_LEN)).is_err());
        assert!(Pairing::parse(&format!("{}-", "0".repeat(BASE62_CODE_LEN - 1))).is_err());
    }

    #[test]
    fn address_classification_separates_local_and_cgnat() {
        assert!(is_local(Ipv4Addr::new(192, 168, 1, 3)));
        assert!(is_local(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_local(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(!is_local(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(!is_local(Ipv4Addr::new(8, 8, 8, 8)));

        assert!(is_cgnat(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_cgnat(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(!is_cgnat(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_cgnat(Ipv4Addr::new(100, 128, 0, 0)));
        assert!(!is_cgnat(Ipv4Addr::new(192, 168, 1, 3)));

        assert!(is_allowed_ip(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_allowed_ip(Ipv4Addr::new(100, 127, 255, 254)));
        assert!(is_allowed_ip(Ipv4Addr::new(192, 168, 1, 3)));
        assert!(!is_allowed_ip(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_allowed_ip(Ipv4Addr::new(100, 128, 0, 0)));
        assert!(!is_allowed_ip(Ipv4Addr::new(8, 8, 8, 8)));

        assert_eq!(
            endpoint("100.64.0.10").unwrap(),
            SocketAddrV4::new(Ipv4Addr::new(100, 64, 0, 10), DEFAULT_PORT)
        );
    }
}
