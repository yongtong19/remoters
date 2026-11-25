use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig,
    ServerConnection, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{DEFAULT_PORT, Desktop, Viewer, session};

const TIMEOUT: Duration = Duration::from_secs(7);

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

    fn code(&self) -> String {
        let mut bytes = [0; 56];
        bytes[..32].copy_from_slice(&self.fingerprint);
        bytes[32..].copy_from_slice(&self.secret);
        URL_SAFE_NO_PAD.encode(bytes)
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

fn require_local(address: SocketAddrV4) -> Result<()> {
    ensure!(
        is_local(*address.ip()),
        "Use a private, link-local, or loopback IPv4 address."
    );
    ensure!(address.port() != 0, "Enter a port from 1 to 65535.");
    Ok(())
}

pub type LanViewer = Viewer<StreamOwned<ClientConnection, TcpStream>>;

pub fn connect(address: SocketAddrV4, pairing: Pairing) -> Result<LanViewer> {
    require_local(address)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinVerifier {
        fingerprint: pairing.fingerprint,
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();

    let socket = TcpStream::connect_timeout(&SocketAddr::V4(address), TIMEOUT)
        .with_context(|| format!("Could not connect to {address}."))?;
    configure(&socket)?;
    let name = ServerName::IpAddress((*address.ip()).into());
    let tls = ClientConnection::new(Arc::new(config), name)?;
    Viewer::authenticate(StreamOwned::new(tls, socket), &pairing.secret)
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

fn configure(socket: &TcpStream) -> io::Result<()> {
    // Some platforms inherit nonblocking mode from the listening socket.
    socket.set_nonblocking(false)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))
}

#[derive(Debug)]
pub enum HostEvent {
    Connected(SocketAddr),
    Finished(Result<()>),
    Stopped,
}

#[derive(Default)]
struct Stop {
    set: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

pub struct Host {
    pub address: SocketAddrV4,
    pub code: String,
    stop: Arc<Stop>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.set.store(true, Ordering::Release);
        if let Some(socket) = self.stop.socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

impl Host {
    pub fn start<D: Desktop>(
        address: SocketAddrV4,
        view_only: bool,
        make_desktop: impl Fn() -> Result<D> + Send + 'static,
        event: impl Fn(HostEvent) + Send + 'static,
    ) -> Result<Self> {
        ensure!(
            is_local(*address.ip()),
            "Use a private, link-local, or loopback IPv4 address."
        );
        let listener = TcpListener::bind(address)
            .with_context(|| format!("Could not listen on {address}."))?;
        listener.set_nonblocking(true)?;
        let SocketAddr::V4(address) = listener.local_addr()? else {
            unreachable!("listener was bound with an IPv4 address");
        };

        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["lan-remote.local".into()])?;
        let mut secret = [0; 24];
        getrandom::fill(&mut secret)
            .map_err(|error| anyhow!("Pairing randomness unavailable: {error}"))?;
        let pairing = Pairing {
            fingerprint: Sha256::digest(cert.der().as_ref()).into(),
            secret,
        };
        let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der()));
        let tls =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(vec![cert.der().clone()], private)?;

        let code = pairing.code();
        let stop = Arc::new(Stop::default());
        let worker_stop = stop.clone();
        std::thread::Builder::new()
            .name("lan-host".into())
            .spawn(move || {
                run_host(
                    listener,
                    Arc::new(tls),
                    pairing,
                    view_only,
                    worker_stop,
                    make_desktop,
                    event,
                );
            })?;
        Ok(Self {
            address,
            code,
            stop,
        })
    }
}

fn run_host<D: Desktop>(
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    pairing: Pairing,
    view_only: bool,
    stop: Arc<Stop>,
    desktop: impl Fn() -> Result<D>,
    event: impl Fn(HostEvent),
) {
    while !stop.set.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((socket, address)) => {
                let SocketAddr::V4(peer) = address else {
                    continue;
                };
                if !is_local(*peer.ip()) {
                    continue;
                }
                let result = (|| {
                    configure(&socket)?;
                    {
                        let mut active = stop.socket.lock().unwrap();
                        if stop.set.load(Ordering::Acquire) {
                            bail!("Sharing was stopped.");
                        }
                        *active = Some(socket.try_clone()?);
                    }
                    let connection = ServerConnection::new(tls.clone())?;
                    session::serve(
                        StreamOwned::new(connection, socket),
                        &pairing.secret,
                        view_only,
                        || {
                            let backend = desktop()?;
                            event(HostEvent::Connected(address));
                            Ok(backend)
                        },
                    )
                })();
                stop.socket.lock().unwrap().take();
                if !stop.set.load(Ordering::Acquire) {
                    event(HostEvent::Finished(result));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(35));
            }
            Err(error) => {
                event(HostEvent::Finished(Err(error.into())));
                break;
            }
        }
    }
    event(HostEvent::Stopped);
}
