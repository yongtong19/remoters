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

pub struct Pairing {
    fingerprint: [u8; 32],
    secret: [u8; 24],
}

impl Pairing {
    pub fn parse(code: &str) -> Result<Self> {
        let code: [u8; 56] = URL_SAFE_NO_PAD
            .decode(code.trim())
            .map_err(|_| anyhow!("Invalid pairing code."))?
            .try_into()
            .map_err(|_| anyhow!("Invalid pairing code."))?;
        let (fingerprint, secret) = code.split_at(32);
        Ok(Self {
            fingerprint: fingerprint.try_into()?,
            secret: secret.try_into()?,
        })
    }

    pub(crate) fn code(&self) -> String {
        let mut bytes = [0; 56];
        bytes[..32].copy_from_slice(&self.fingerprint);
        bytes[32..].copy_from_slice(&self.secret);
        URL_SAFE_NO_PAD.encode(bytes)
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
    require_local(address)?;
    Ok(address)
}

pub fn is_local(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}

pub(crate) fn require_local(address: SocketAddrV4) -> Result<()> {
    ensure!(
        is_local(*address.ip()),
        "Use a private, link-local, or loopback IPv4 address."
    );
    ensure!(address.port() != 0, "Enter a port from 1 to 65535.");
    Ok(())
}

pub(crate) fn is_local_peer(address: SocketAddr) -> bool {
    matches!(address, SocketAddr::V4(address) if is_local(*address.ip()))
}

/// Finds the local address selected by the OS for an IPv4 route without sending a packet.
pub(crate) fn local_route(address: SocketAddrV4) -> Result<Ipv4Addr> {
    require_local(address)?;
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
