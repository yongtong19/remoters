use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, Response, StatusCode,
    body::Incoming,
    header::{AUTHORIZATION, CONTENT_TYPE},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use remoters_core::account::{
    ApiErrorResponse, DeviceHeartbeat, DeviceResponse, DevicesResponse, HealthResponse,
    LoginRequest, MeResponse, RegisterRequest, decode_path_segment,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

use crate::{
    config::ServerConfig,
    store::{AccountStore, StoreError},
};

const MAX_BODY_BYTES: usize = 64 * 1024;
const JSON_CONTENT_TYPE: &str = "application/json";

/// Running HTTP/1.1 account & device directory server.
pub struct AccountServer {
    local_addr: SocketAddr,
    store: Arc<AccountStore>,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl AccountServer {
    pub async fn bind(config: ServerConfig) -> Result<Self> {
        let store = Arc::new(AccountStore::open(&config.database).await?);
        let listener = TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("Could not bind account server on {}.", config.bind))?;
        let local_addr = listener.local_addr()?;
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let task_store = store.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((stream, _peer)) = accepted else {
                            break;
                        };
                        let conn_store = task_store.clone();
                        let mut conn_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            let service = service_fn(move |req| {
                                let store = conn_store.clone();
                                async move {
                                    Ok::<_, Infallible>(handle_request(req, &store).await)
                                }
                            });
                            let conn = hyper::server::conn::http1::Builder::new()
                                .keep_alive(false)
                                .serve_connection(TokioIo::new(stream), service);
                            tokio::pin!(conn);
                            tokio::select! {
                                _ = &mut conn => {}
                                _ = conn_shutdown.changed() => {
                                    conn.as_mut().graceful_shutdown();
                                    let _ = conn.await;
                                }
                            }
                        });
                    }
                }
            }
        });

        Ok(Self {
            local_addr,
            store,
            shutdown_tx,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn store(&self) -> &Arc<AccountStore> {
        &self.store
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.task.await;
    }
}

async fn handle_request(request: Request<Incoming>, store: &AccountStore) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let method = parts.method;

    match (method.clone(), path) {
        (Method::GET, "/health") => json_response(
            StatusCode::OK,
            &HealthResponse {
                status: "ok".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        ),
        (Method::POST, "/api/v1/auth/register") => {
            let payload: RegisterRequest = match read_json(body).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match store
                .register(
                    &payload.username,
                    &payload.password,
                    payload.display_name.as_deref(),
                )
                .await
            {
                Ok(session) => json_response(StatusCode::CREATED, &session),
                Err(err) => store_error_response(err),
            }
        }
        (Method::POST, "/api/v1/auth/login") => {
            let payload: LoginRequest = match read_json(body).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match store.login(&payload.username, &payload.password).await {
                Ok(session) => json_response(StatusCode::OK, &session),
                Err(err) => store_error_response(err),
            }
        }
        (Method::POST, "/api/v1/auth/logout") => {
            let token = match bearer_token(&parts.headers) {
                Ok(t) => t,
                Err(resp) => return resp,
            };
            match store.logout(token).await {
                Ok(()) => empty_response(StatusCode::NO_CONTENT),
                Err(err) => store_error_response(err),
            }
        }
        (Method::GET, "/api/v1/auth/me") => {
            let token = match bearer_token(&parts.headers) {
                Ok(t) => t,
                Err(resp) => return resp,
            };
            match store.authenticate(token).await {
                Ok(user) => json_response(StatusCode::OK, &MeResponse { user }),
                Err(err) => store_error_response(err),
            }
        }
        (Method::POST, "/api/v1/devices/heartbeat") => {
            let token = match bearer_token(&parts.headers) {
                Ok(t) => t,
                Err(resp) => return resp,
            };
            let payload: DeviceHeartbeat = match read_json(body).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            match store.heartbeat(token, payload).await {
                Ok(device) => json_response(StatusCode::OK, &DeviceResponse { device }),
                Err(err) => store_error_response(err),
            }
        }
        (Method::GET, "/api/v1/devices") => {
            let token = match bearer_token(&parts.headers) {
                Ok(t) => t,
                Err(resp) => return resp,
            };
            match store.list_devices(token).await {
                Ok(devices) => json_response(StatusCode::OK, &DevicesResponse { devices }),
                Err(err) => store_error_response(err),
            }
        }
        (Method::DELETE, path) if path.starts_with("/api/v1/devices/") => {
            let encoded_id = &path["/api/v1/devices/".len()..];
            if encoded_id.is_empty() || encoded_id.contains('/') {
                return error_response(StatusCode::NOT_FOUND, "Device endpoint not found.");
            }
            let Some(device_id) = decode_path_segment(encoded_id) else {
                return error_response(StatusCode::BAD_REQUEST, "Invalid device ID encoding.");
            };
            let token = match bearer_token(&parts.headers) {
                Ok(t) => t,
                Err(resp) => return resp,
            };
            match store.delete_device(token, &device_id).await {
                Ok(()) => empty_response(StatusCode::NO_CONTENT),
                Err(err) => store_error_response(err),
            }
        }
        (_, "/health")
        | (_, "/api/v1/auth/register")
        | (_, "/api/v1/auth/login")
        | (_, "/api/v1/auth/logout")
        | (_, "/api/v1/auth/me")
        | (_, "/api/v1/devices/heartbeat")
        | (_, "/api/v1/devices") => {
            error_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed.")
        }
        (_, p) if p.starts_with("/api/v1/devices/") => {
            error_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed.")
        }
        _ => error_response(StatusCode::NOT_FOUND, "Not found."),
    }
}

fn bearer_token(headers: &hyper::HeaderMap) -> std::result::Result<&str, Response<Full<Bytes>>> {
    let Some(header) = headers.get(AUTHORIZATION) else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Missing Authorization Bearer token.",
        ));
    };
    let Ok(value) = header.to_str() else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid Authorization header.",
        ));
    };
    let Some(token) = value.strip_prefix("Bearer ") else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Authorization header must use the Bearer scheme.",
        ));
    };
    if token.trim().is_empty() {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "Bearer token cannot be empty.",
        ));
    }
    Ok(token.trim())
}

async fn read_json<T: DeserializeOwned>(
    body: Incoming,
) -> std::result::Result<T, Response<Full<Bytes>>> {
    let bytes = Limited::new(body, MAX_BODY_BYTES)
        .collect()
        .await
        .map_err(|_| {
            error_response(
                StatusCode::BAD_REQUEST,
                "Request body is too large or unreadable.",
            )
        })?
        .to_bytes();
    serde_json::from_slice(&bytes).map_err(|error| {
        error_response(
            StatusCode::BAD_REQUEST,
            format!("Invalid JSON body: {error}"),
        )
    })
}

fn store_error_response(error: StoreError) -> Response<Full<Bytes>> {
    match error {
        StoreError::BadRequest(msg) => error_response(StatusCode::BAD_REQUEST, msg),
        StoreError::Unauthorized(msg) => error_response(StatusCode::UNAUTHORIZED, msg),
        StoreError::Conflict(msg) => error_response(StatusCode::CONFLICT, msg),
        StoreError::NotFound(msg) => error_response(StatusCode::NOT_FOUND, msg),
        StoreError::Internal(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.")
        }
    }
}

fn json_response<T: Serialize>(status: StatusCode, body: &T) -> Response<Full<Bytes>> {
    match serde_json::to_vec(body) {
        Ok(bytes) => {
            let mut response = Response::new(Full::new(Bytes::from(bytes)));
            *response.status_mut() = status;
            response.headers_mut().insert(
                CONTENT_TYPE,
                hyper::header::HeaderValue::from_static(JSON_CONTENT_TYPE),
            );
            response
        }
        Err(_) => empty_response(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response<Full<Bytes>> {
    json_response(
        status,
        &ApiErrorResponse {
            error: message.into(),
        },
    )
}

fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}
