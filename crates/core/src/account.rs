use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Method, Request, StatusCode, Uri,
    client::conn::http1,
    header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST},
};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::{net::TcpStream, time::timeout};

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8787";
pub const ONLINE_WINDOW_MS: u64 = 45_000;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const JSON_CONTENT_TYPE: &str = "application/json";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Authenticated user profile returned by the account server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountUser {
    pub id: String,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at_unix_ms: u64,
}

/// Active bearer session returned by `/api/v1/auth/register` and `/api/v1/auth/login`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthSession {
    pub token: String,
    pub user: AccountUser,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_ms: Option<u64>,
}

/// Device status heartbeat posted to `/api/v1/devices/heartbeat`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceHeartbeat {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    #[serde(default)]
    pub host_address: Option<String>,
    #[serde(default)]
    pub pairing_code: Option<String>,
    pub sharing: bool,
    pub view_only: bool,
    #[serde(default)]
    pub display_label: String,
}

/// Device record returned by `/api/v1/devices/heartbeat` and `/api/v1/devices`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    #[serde(default)]
    pub host_address: Option<String>,
    #[serde(default)]
    pub pairing_code: Option<String>,
    pub sharing: bool,
    pub view_only: bool,
    #[serde(default)]
    pub display_label: String,
    pub last_seen_unix_ms: u64,
    pub online: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MeResponse {
    pub user: AccountUser,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceResponse {
    pub device: DeviceRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    pub error: String,
}

/// Structured error returned inside `anyhow::Error` by `AccountClient`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountError {
    Api { status: u16, message: String },
    Transport(String),
    InvalidUrl(String),
    InvalidResponse(String),
}

impl AccountError {
    /// Returns `true` when the server rejected authentication with HTTP 401.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Api { status: 401, .. })
    }
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { message, .. } => f.write_str(message),
            Self::Transport(message)
            | Self::InvalidUrl(message)
            | Self::InvalidResponse(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AccountError {}

/// Persistent client-side account state stored in `account.json`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountStorage {
    #[serde(default = "default_server_url")]
    pub server_url: String,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AuthSession>,
}

fn default_server_url() -> String {
    DEFAULT_SERVER_URL.to_owned()
}

impl AccountStorage {
    /// Returns the default `account.json` path under the user's configuration directory.
    pub fn default_path() -> Result<PathBuf> {
        let dir =
            dirs::config_dir().context("Could not determine the user configuration directory.")?;
        Ok(dir.join("remoters").join("account.json"))
    }

    /// Loads `account.json` from the default user configuration directory, initializing a persistent `device_id` if absent.
    pub fn load() -> Result<Self> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Loads `account.json` from `path`, initializing and saving a persistent `device_id` if the file does not exist yet.
    pub fn load_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(raw) => {
                let mut storage: Self = serde_json::from_str(&raw).with_context(|| {
                    format!("Could not parse account storage at {}.", path.display())
                })?;
                let mut changed = false;
                if storage.device_id.trim().is_empty() {
                    storage.device_id = Self::generate_device_id()?;
                    changed = true;
                }
                if storage.server_url.trim().is_empty() {
                    storage.server_url = DEFAULT_SERVER_URL.to_owned();
                    changed = true;
                }
                if changed {
                    storage.save_to(path)?;
                }
                Ok(storage)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let storage = Self {
                    server_url: DEFAULT_SERVER_URL.to_owned(),
                    device_id: Self::generate_device_id()?,
                    session: None,
                };
                storage.save_to(path)?;
                Ok(storage)
            }
            Err(error) => Err(error)
                .with_context(|| format!("Could not read account storage at {}.", path.display())),
        }
    }

    /// Saves `account.json` to the default user configuration directory with `0600` permissions on Unix.
    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Atomically writes `account.json` to `path` with `0600` permissions on Unix.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Could not create account config directory {}.",
                parent.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }

        let payload =
            serde_json::to_vec_pretty(self).context("Could not serialize account storage.")?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("account.json");
        let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".{file_name}.tmp.{}.{seq}", std::process::id()));

        let write_result = (|| -> Result<()> {
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp_path).with_context(|| {
                format!(
                    "Could not create temp account file {}.",
                    temp_path.display()
                )
            })?;
            file.write_all(&payload)?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))?;
            }
            fs::rename(&temp_path, path).with_context(|| {
                format!("Could not replace account storage file {}.", path.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }

    /// Clears any cached authentication session without modifying `device_id` or `server_url`.
    pub fn clear_session(&mut self) {
        self.session = None;
    }

    /// Generates a random UUIDv4-formatted per-machine device identifier.
    pub fn generate_device_id() -> Result<String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow!("Device ID randomness unavailable: {error}"))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15],
        ))
    }
}

/// Async HTTP/1.1 client for the remoters account & device directory server.
#[derive(Clone, Debug)]
pub struct AccountClient {
    base_url: String,
    connect_target: String,
    host_header: String,
    base_path: String,
}

impl AccountClient {
    /// Creates a new `AccountClient` targeting `base_url` (for example `http://127.0.0.1:8787`).
    pub fn new(base_url: impl AsRef<str>) -> Result<Self> {
        let trimmed = base_url.as_ref().trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(AccountError::InvalidUrl(
                "Enter an account server URL (for example http://127.0.0.1:8787).".to_owned(),
            )
            .into());
        }
        let with_scheme = if trimmed.contains("://") {
            trimmed.to_owned()
        } else {
            format!("http://{trimmed}")
        };
        let uri: Uri = with_scheme.parse().map_err(|error| {
            AccountError::InvalidUrl(format!("Invalid account server URL `{trimmed}`: {error}"))
        })?;
        match uri.scheme_str() {
            Some("http") => {}
            Some(scheme) => {
                return Err(AccountError::InvalidUrl(format!(
                    "Unsupported account server scheme `{scheme}://`; only `http://` URLs are supported."
                ))
                .into());
            }
            None => {
                return Err(AccountError::InvalidUrl(
                    "Account server URL must include a scheme (`http://`).".to_owned(),
                )
                .into());
            }
        }
        if uri.query().is_some() {
            return Err(AccountError::InvalidUrl(
                "Account server URL must not include a query string.".to_owned(),
            )
            .into());
        }
        let authority = uri.authority().ok_or_else(|| {
            AccountError::InvalidUrl("Account server URL must include a host.".to_owned())
        })?;
        if authority.as_str().contains('@') {
            return Err(AccountError::InvalidUrl(
                "Account server URL must not include userinfo.".to_owned(),
            )
            .into());
        }
        let host = authority.host();
        if host.is_empty() {
            return Err(AccountError::InvalidUrl(
                "Account server URL must include a host.".to_owned(),
            )
            .into());
        }
        let port = authority.port_u16().unwrap_or(80);
        let formatted_host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let connect_target = format!("{formatted_host}:{port}");
        let host_header = authority.as_str().to_owned();
        let base_path = uri.path().trim_end_matches('/').to_owned();
        let normalized_url = if base_path.is_empty() {
            format!("http://{host_header}")
        } else {
            format!("http://{host_header}{base_path}")
        };

        Ok(Self {
            base_url: normalized_url,
            connect_target,
            host_header,
            base_path,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn health(&self) -> Result<HealthResponse> {
        self.request_json::<(), HealthResponse>(Method::GET, "/health", None, None)
            .await
    }

    pub async fn register(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> Result<AuthSession> {
        let body = RegisterRequest {
            username: username.to_owned(),
            password: password.to_owned(),
            display_name: display_name.map(ToOwned::to_owned),
        };
        self.request_json(Method::POST, "/api/v1/auth/register", None, Some(&body))
            .await
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<AuthSession> {
        let body = LoginRequest {
            username: username.to_owned(),
            password: password.to_owned(),
        };
        self.request_json(Method::POST, "/api/v1/auth/login", None, Some(&body))
            .await
    }

    pub async fn logout(&self, token: &str) -> Result<()> {
        self.request_empty(Method::POST, "/api/v1/auth/logout", Some(token))
            .await
    }

    pub async fn me(&self, token: &str) -> Result<AccountUser> {
        let response: MeResponse = self
            .request_json::<(), MeResponse>(Method::GET, "/api/v1/auth/me", Some(token), None)
            .await?;
        Ok(response.user)
    }

    pub async fn heartbeat(
        &self,
        token: &str,
        heartbeat: &DeviceHeartbeat,
    ) -> Result<DeviceRecord> {
        let response: DeviceResponse = self
            .request_json(
                Method::POST,
                "/api/v1/devices/heartbeat",
                Some(token),
                Some(heartbeat),
            )
            .await?;
        Ok(response.device)
    }

    pub async fn list_devices(&self, token: &str) -> Result<Vec<DeviceRecord>> {
        let response: DevicesResponse = self
            .request_json::<(), DevicesResponse>(Method::GET, "/api/v1/devices", Some(token), None)
            .await?;
        Ok(response.devices)
    }

    pub async fn delete_device(&self, token: &str, device_id: &str) -> Result<()> {
        let encoded = encode_path_segment(device_id);
        let path = format!("/api/v1/devices/{encoded}");
        self.request_empty(Method::DELETE, &path, Some(token)).await
    }

    async fn request_json<Req: Serialize, Resp: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        body: Option<&Req>,
    ) -> Result<Resp> {
        let payload = match body {
            Some(value) => {
                serde_json::to_vec(value).context("Could not serialize account request payload.")?
            }
            None => Vec::new(),
        };
        let (status, bytes) = self.send_raw(method, path, bearer, payload).await?;
        if !status.is_success() {
            return Err(parse_error_response(status, &bytes).into());
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            AccountError::InvalidResponse(format!(
                "Account server returned an unexpected response: {error}"
            ))
            .into()
        })
    }

    async fn request_empty(&self, method: Method, path: &str, bearer: Option<&str>) -> Result<()> {
        let (status, bytes) = self.send_raw(method, path, bearer, Vec::new()).await?;
        if !status.is_success() {
            return Err(parse_error_response(status, &bytes).into());
        }
        Ok(())
    }

    async fn send_raw(
        &self,
        method: Method,
        path: &str,
        bearer: Option<&str>,
        payload: Vec<u8>,
    ) -> Result<(StatusCode, Bytes)> {
        let full_path = if self.base_path.is_empty() {
            path.to_owned()
        } else {
            format!("{}{}", self.base_path, path)
        };
        let has_body = !payload.is_empty();
        let mut builder = Request::builder()
            .method(method)
            .uri(&full_path)
            .header(HOST, &self.host_header)
            .header(CONNECTION, "close");
        if let Some(token) = bearer {
            builder = builder.header(AUTHORIZATION, format!("Bearer {}", token.trim()));
        }
        if has_body {
            builder = builder.header(CONTENT_TYPE, JSON_CONTENT_TYPE);
        }
        let request = builder
            .body(Full::new(Bytes::from(payload)))
            .context("Could not build account HTTP request.")?;

        let base_url = &self.base_url;
        let connect_target = &self.connect_target;

        timeout(REQUEST_TIMEOUT, async {
            let stream = TcpStream::connect(connect_target).await.map_err(|error| {
                AccountError::Transport(format!(
                    "Could not reach account server at {base_url}: {error}"
                ))
            })?;
            let (mut sender, connection) =
                http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|error| {
                        AccountError::Transport(format!(
                            "Account server HTTP handshake failed at {base_url}: {error}"
                        ))
                    })?;

            let send = async move {
                let response = sender.send_request(request).await.map_err(|error| {
                    AccountError::Transport(format!(
                        "Account server request failed at {base_url}: {error}"
                    ))
                })?;
                let status = response.status();
                let collected = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
                    .collect()
                    .await
                    .map_err(|error| {
                        AccountError::InvalidResponse(format!(
                            "Could not read account server response: {error}"
                        ))
                    })?;
                Ok::<_, AccountError>((status, collected.to_bytes()))
            };

            let (response_result, conn_result) = tokio::join!(send, connection);
            let outcome = response_result?;
            if let Err(error) = conn_result {
                return Err(AccountError::Transport(format!(
                    "Account server connection error at {base_url}: {error}"
                ))
                .into());
            }
            Ok(outcome)
        })
        .await
        .map_err(|_| {
            anyhow::Error::from(AccountError::Transport(format!(
                "Account server request timed out ({base_url})."
            )))
        })?
    }
}

fn parse_error_response(status: StatusCode, body: &[u8]) -> AccountError {
    if let Ok(parsed) = serde_json::from_slice::<ApiErrorResponse>(body) {
        let trimmed = parsed.error.trim();
        if !trimmed.is_empty() {
            return AccountError::Api {
                status: status.as_u16(),
                message: trimmed.to_owned(),
            };
        }
    }
    let reason = status.canonical_reason().unwrap_or("Request failed");
    AccountError::Api {
        status: status.as_u16(),
        message: format!(
            "Account server returned HTTP {} ({reason}).",
            status.as_u16()
        ),
    }
}

/// Percent-encodes a single URI path segment (RFC 3986 unreserved characters kept raw).
pub fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_char(byte >> 4));
            out.push(hex_char(byte & 0x0f));
        }
    }
    out
}

/// Percent-decodes a single URI path segment.
pub fn decode_path_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = from_hex(bytes[i + 1])?;
            let lo = from_hex(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_char(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!(),
    }
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AccountClient, AccountStorage, decode_path_segment, encode_path_segment};

    #[test]
    fn path_segment_round_trip() {
        let raw = "macbook pro/123?a=b#c";
        let encoded = encode_path_segment(raw);
        assert_eq!(decode_path_segment(&encoded).as_deref(), Some(raw));
        assert!(decode_path_segment("%zz").is_none());
    }

    #[test]
    fn account_client_url_normalization_and_validation() {
        let client = AccountClient::new("http://127.0.0.1:8787/").unwrap();
        assert_eq!(client.base_url(), "http://127.0.0.1:8787");

        let bare = AccountClient::new("127.0.0.1:8787").unwrap();
        assert_eq!(bare.base_url(), "http://127.0.0.1:8787");

        assert!(AccountClient::new("https://example.com").is_err());
        assert!(AccountClient::new("").is_err());
        assert!(AccountClient::new("http://127.0.0.1:8787?foo=1").is_err());
    }

    #[test]
    fn account_storage_persists_device_id_and_permissions() {
        let dir = PathBuf::from(format!(
            "{}/remoters-account-test-{}-{}",
            std::env::temp_dir().display(),
            std::process::id(),
            super::TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let path = dir.join("account.json");

        let loaded = AccountStorage::load_from(&path).unwrap();
        assert!(!loaded.device_id.is_empty());
        assert_eq!(loaded.server_url, super::DEFAULT_SERVER_URL);
        assert!(loaded.session.is_none());

        let reloaded = AccountStorage::load_from(&path).unwrap();
        assert_eq!(reloaded.device_id, loaded.device_id);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
