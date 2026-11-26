use std::{
    convert::Infallible,
    net::{SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    client::conn::http1,
    header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use rustls::{ClientConfig, ServerConfig, crypto::ring::default_provider};
use subtle::ConstantTimeEq;
use tokio::{
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::{Pairing, is_allowed_ip, is_allowed_peer, require_allowed};

const SESSION: &str = "/session";
const SDP: &str = "application/sdp";
const MAX_SDP: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(7);

pub(crate) struct SignalingHost {
    listener: TcpListener,
    tls: TlsAcceptor,
    pairing: Pairing,
}

impl SignalingHost {
    pub fn bind(address: SocketAddrV4) -> Result<Self> {
        ensure!(
            is_allowed_ip(*address.ip()),
            "Use a private, shared VPN (100.64.0.0/10), link-local, or loopback IPv4 address."
        );
        let (pairing, certificate, key) = Pairing::identity()?;
        let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)?;
        let listener = std::net::TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: TcpListener::from_std(listener)?,
            tls: TlsAcceptor::from(Arc::new(config)),
            pairing,
        })
    }

    pub fn address(&self) -> Result<SocketAddrV4> {
        match self.listener.local_addr()? {
            SocketAddr::V4(address) => Ok(address),
            _ => anyhow::bail!("Expected an IPv4 signaling endpoint."),
        }
    }

    pub fn pairing_code(&self) -> String {
        self.pairing.code()
    }

    pub async fn accept(
        &self,
        answer: impl FnOnce(String) -> Result<String>,
    ) -> Result<SocketAddr> {
        let (socket, peer) = self.listener.accept().await?;
        ensure!(
            is_allowed_peer(peer),
            "The signaling peer must use a private, shared VPN, link-local, or loopback IPv4 address."
        );
        let expected = bearer(&self.pairing);
        let answer = Mutex::new(Some(answer));
        let result = Mutex::new(None);

        timeout(CONNECT_TIMEOUT, async {
            let tls = self.tls.accept(socket).await?;
            hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .serve_connection(
                    TokioIo::new(tls),
                    service_fn(|request| async {
                        let response = handle_offer(request, &expected, &answer, &result).await;
                        Ok::<_, Infallible>(response)
                    }),
                )
                .await?;
            result
                .into_inner()
                .unwrap()
                .context("The peer did not send an authenticated SDP offer.")??;
            Ok(peer)
        })
        .await?
    }
}

pub(crate) async fn offer(address: SocketAddrV4, pairing: &Pairing, sdp: String) -> Result<String> {
    require_allowed(address)?;
    timeout(CONNECT_TIMEOUT, async {
        let provider = Arc::new(default_provider());
        let verifier = pairing.verifier(provider.clone());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let socket = TcpStream::connect(address).await?;
        let name = rustls::pki_types::ServerName::IpAddress((*address.ip()).into());
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, socket)
            .await?;
        let (mut client, connection) = http1::handshake(TokioIo::new(tls)).await?;
        let request = Request::post(SESSION)
            .header(HOST, address.to_string())
            .header(CONNECTION, "close")
            .header(AUTHORIZATION, bearer(pairing))
            .header(CONTENT_TYPE, SDP)
            .body(Full::new(Bytes::from(sdp)))?;
        // Drive the HTTP connection and its body together; cancellation drops both.
        let send = async {
            let response = client.send_request(request).await?;
            ensure!(
                response.status() == StatusCode::OK,
                "The host rejected the WebRTC offer (HTTP {}).",
                response.status()
            );
            ensure!(
                response
                    .headers()
                    .get(CONTENT_TYPE)
                    .is_some_and(|value| value == SDP),
                "Expected an SDP answer."
            );
            read_sdp(response.into_body()).await
        };
        let (answer, connection) = tokio::join!(send, connection);
        let answer = answer?;
        connection?;
        Ok(answer)
    })
    .await?
}

async fn handle_offer(
    request: Request<Incoming>,
    expected: &str,
    answer: &Mutex<Option<impl FnOnce(String) -> Result<String>>>,
    result: &Mutex<Option<Result<()>>>,
) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    if parts.method != Method::POST || parts.uri.path() != SESSION {
        return response(StatusCode::NOT_FOUND, "");
    }
    if !parts
        .headers
        .get(AUTHORIZATION)
        .is_some_and(|value| bool::from(value.as_bytes().ct_eq(expected.as_bytes())))
    {
        return response(StatusCode::FORBIDDEN, "");
    }
    if !parts
        .headers
        .get(CONTENT_TYPE)
        .is_some_and(|value| value == SDP)
    {
        return response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "");
    }
    let outcome = async {
        let offer = read_sdp(body).await?;
        let answer = answer
            .lock()
            .unwrap()
            .take()
            .context("Only one offer is accepted.")?;
        answer(offer)
    }
    .await;
    match outcome {
        Ok(sdp) => {
            *result.lock().unwrap() = Some(Ok(()));
            response(StatusCode::OK, sdp)
        }
        Err(error) => {
            *result.lock().unwrap() = Some(Err(error));
            response(StatusCode::BAD_REQUEST, "")
        }
    }
}

fn bearer(pairing: &Pairing) -> String {
    format!("Bearer {}", URL_SAFE_NO_PAD.encode(pairing.secret()))
}

async fn read_sdp(body: Incoming) -> Result<String> {
    let bytes = Limited::new(body, MAX_SDP)
        .collect()
        .await
        .map_err(|error| anyhow::anyhow!("Could not read the SDP body: {error}"))?
        .to_bytes();
    let sdp = std::str::from_utf8(&bytes)?;
    Ok(sdp.to_owned())
}

fn response(status: StatusCode, sdp: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(sdp.into()));
    *response.status_mut() = status;
    if status == StatusCode::OK {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, hyper::header::HeaderValue::from_static(SDP));
    }
    response
}
