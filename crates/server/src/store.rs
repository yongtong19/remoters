use std::{
    cmp::Reverse,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::pbkdf2_hmac;
use remoters_core::account::{
    AccountUser, AuthSession, DeviceHeartbeat, DeviceRecord, ONLINE_WINDOW_MS,
};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Row, SqlitePool,
    postgres::PgPoolOptions,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use subtle::ConstantTimeEq;

use crate::config::DatabaseConfig;

pub const PBKDF2_ROUNDS: u32 = 100_000;
pub const SESSION_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

const SALT_BYTES: usize = 16;
const HASH_BYTES: usize = 32;
const TOKEN_BYTES: usize = 32;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_DISPLAY_NAME_CHARS: usize = 64;

const SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS users (
        id TEXT PRIMARY KEY,
        username TEXT NOT NULL,
        username_lower TEXT NOT NULL UNIQUE,
        display_name TEXT,
        password_salt_b64 TEXT NOT NULL,
        password_hash_b64 TEXT NOT NULL,
        created_at_unix_ms BIGINT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS sessions (
        token_hash_b64 TEXT PRIMARY KEY,
        user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        created_at_unix_ms BIGINT NOT NULL,
        expires_at_unix_ms BIGINT NOT NULL
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id)",
    "CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions(expires_at_unix_ms)",
    r#"CREATE TABLE IF NOT EXISTS devices (
        user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
        device_id TEXT NOT NULL,
        name TEXT NOT NULL,
        platform TEXT NOT NULL,
        host_address TEXT,
        pairing_code TEXT,
        sharing BOOLEAN NOT NULL,
        view_only BOOLEAN NOT NULL,
        display_label TEXT NOT NULL,
        last_seen_unix_ms BIGINT NOT NULL,
        PRIMARY KEY (user_id, device_id)
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id)",
];

#[derive(Debug)]
pub enum StoreError {
    BadRequest(String),
    Unauthorized(String),
    Conflict(String),
    NotFound(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for StoreError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value)
    }
}

impl From<sqlx::Error> for StoreError {
    fn from(value: sqlx::Error) -> Self {
        Self::Internal(value.into())
    }
}

enum Backend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

struct UserCredentialsRow {
    id: String,
    username: String,
    display_name: Option<String>,
    password_salt_b64: String,
    password_hash_b64: String,
    created_at_unix_ms: i64,
}

impl UserCredentialsRow {
    fn into_public(self) -> AccountUser {
        AccountUser {
            id: self.id,
            username: self.username,
            display_name: self.display_name,
            created_at_unix_ms: u64::try_from(self.created_at_unix_ms).unwrap_or_default(),
        }
    }
}

macro_rules! with_pool {
    ($self:expr, |$pool:ident| $body:expr) => {
        match &$self.backend {
            Backend::Sqlite($pool) => $body,
            Backend::Postgres($pool) => $body,
        }
    };
}

/// SQLite- or PostgreSQL-backed store for accounts, hashed session tokens, and devices.
pub struct AccountStore {
    backend: Backend,
}

impl AccountStore {
    /// Opens or initializes the configured SQLite or PostgreSQL database.
    pub async fn open(config: &DatabaseConfig) -> Result<Self> {
        let backend = match config {
            DatabaseConfig::Sqlite { path, in_memory } => {
                let pool = if *in_memory {
                    let options = SqliteConnectOptions::from_str("sqlite::memory:")?
                        .foreign_keys(true)
                        .busy_timeout(Duration::from_secs(5));
                    SqlitePoolOptions::new()
                        .max_connections(1)
                        .connect_with(options)
                        .await
                        .context("Could not initialize in-memory SQLite store.")?
                } else {
                    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        tokio::fs::create_dir_all(parent).await.with_context(|| {
                            format!(
                                "Could not create SQLite database directory {}.",
                                parent.display()
                            )
                        })?;
                    }
                    let options = SqliteConnectOptions::new()
                        .filename(path)
                        .create_if_missing(true)
                        .journal_mode(SqliteJournalMode::Wal)
                        .foreign_keys(true)
                        .busy_timeout(Duration::from_secs(5));
                    SqlitePoolOptions::new()
                        .max_connections(8)
                        .connect_with(options)
                        .await
                        .with_context(|| {
                            format!("Could not open SQLite database at {}.", path.display())
                        })?
                };
                Backend::Sqlite(pool)
            }
            DatabaseConfig::Postgres { url } => {
                let pool = PgPoolOptions::new()
                    .max_connections(16)
                    .acquire_timeout(Duration::from_secs(10))
                    .connect(url)
                    .await
                    .context("Could not connect to PostgreSQL database.")?;
                Backend::Postgres(pool)
            }
        };

        let store = Self { backend };
        store.init_schema().await?;
        Ok(store)
    }

    /// Parses a path or database URL and opens the corresponding store.
    pub async fn open_url(raw: &str) -> Result<Self> {
        let config = DatabaseConfig::parse(raw)?;
        Self::open(&config).await
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Sqlite(_) => "sqlite",
            Backend::Postgres(_) => "postgres",
        }
    }

    async fn init_schema(&self) -> Result<()> {
        for ddl in SCHEMA_STATEMENTS {
            with_pool!(self, |pool| {
                sqlx::query(ddl)
                    .execute(pool)
                    .await
                    .with_context(|| format!("Failed to initialize database schema: {ddl}"))?;
            });
        }
        Ok(())
    }

    pub async fn register(
        &self,
        username: &str,
        password: &str,
        display_name: Option<&str>,
    ) -> std::result::Result<AuthSession, StoreError> {
        let username = validate_username(username)?.to_owned();
        let username_lower = username.to_ascii_lowercase();
        validate_password(password)?;
        let display_name = normalize_display_name(display_name)?;
        let now_ms = unix_now_ms();
        let expires_at_unix_ms = now_ms.saturating_add(SESSION_TTL_MS);
        let now_i64 = to_db_timestamp(now_ms)?;
        let expires_i64 = to_db_timestamp(expires_at_unix_ms)?;

        let password_owned = password.to_owned();
        let (salt_b64, hash_b64) =
            tokio::task::spawn_blocking(move || hash_password(&password_owned))
                .await
                .map_err(|error| anyhow!("Password hashing task failed: {error}"))??;
        let user_id = random_id("usr")?;
        let (raw_token, token_hash_b64) = generate_session_token()?;

        with_pool!(self, |pool| {
            let mut tx = pool.begin().await?;
            sqlx::query("DELETE FROM sessions WHERE expires_at_unix_ms <= $1")
                .bind(now_i64)
                .execute(&mut *tx)
                .await?;

            let insert_user = sqlx::query(
                "INSERT INTO users (id, username, username_lower, display_name, password_salt_b64, password_hash_b64, created_at_unix_ms) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&user_id)
            .bind(&username)
            .bind(&username_lower)
            .bind(&display_name)
            .bind(&salt_b64)
            .bind(&hash_b64)
            .bind(now_i64)
            .execute(&mut *tx)
            .await;

            if let Err(error) = insert_user {
                if is_unique_violation(&error) {
                    return Err(StoreError::Conflict(format!(
                        "Username `{username}` is already registered."
                    )));
                }
                return Err(error.into());
            }

            sqlx::query(
                "INSERT INTO sessions (token_hash_b64, user_id, created_at_unix_ms, expires_at_unix_ms) VALUES ($1, $2, $3, $4)",
            )
            .bind(&token_hash_b64)
            .bind(&user_id)
            .bind(now_i64)
            .bind(expires_i64)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
        });

        Ok(AuthSession {
            token: raw_token,
            user: AccountUser {
                id: user_id,
                username,
                display_name,
                created_at_unix_ms: now_ms,
            },
            expires_at_unix_ms: Some(expires_at_unix_ms),
        })
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> std::result::Result<AuthSession, StoreError> {
        let trimmed = username.trim();
        if trimmed.is_empty() || password.is_empty() {
            return Err(StoreError::BadRequest(
                "Username and password are required.".to_owned(),
            ));
        }
        let username_lower = trimmed.to_ascii_lowercase();
        let now_ms = unix_now_ms();
        let expires_at_unix_ms = now_ms.saturating_add(SESSION_TTL_MS);
        let now_i64 = to_db_timestamp(now_ms)?;
        let expires_i64 = to_db_timestamp(expires_at_unix_ms)?;

        let row = with_pool!(self, |pool| {
            sqlx::query(
                "SELECT id, username, display_name, password_salt_b64, password_hash_b64, created_at_unix_ms FROM users WHERE username_lower = $1",
            )
            .bind(&username_lower)
            .fetch_optional(pool)
            .await?
            .map(|r| -> std::result::Result<UserCredentialsRow, sqlx::Error> {
                Ok(UserCredentialsRow {
                    id: r.try_get("id")?,
                    username: r.try_get("username")?,
                    display_name: r.try_get("display_name")?,
                    password_salt_b64: r.try_get("password_salt_b64")?,
                    password_hash_b64: r.try_get("password_hash_b64")?,
                    created_at_unix_ms: r.try_get("created_at_unix_ms")?,
                })
            })
            .transpose()?
        });

        let Some(user) = row else {
            return Err(StoreError::Unauthorized(
                "Invalid username or password.".to_owned(),
            ));
        };

        let password_owned = password.to_owned();
        let salt = user.password_salt_b64.clone();
        let expected_hash = user.password_hash_b64.clone();
        let valid = tokio::task::spawn_blocking(move || {
            verify_password(&password_owned, &salt, &expected_hash)
        })
        .await
        .map_err(|error| anyhow!("Password verification task failed: {error}"))??;

        if !valid {
            return Err(StoreError::Unauthorized(
                "Invalid username or password.".to_owned(),
            ));
        }

        let (raw_token, token_hash_b64) = generate_session_token()?;
        with_pool!(self, |pool| {
            let mut tx = pool.begin().await?;
            sqlx::query("DELETE FROM sessions WHERE expires_at_unix_ms <= $1")
                .bind(now_i64)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO sessions (token_hash_b64, user_id, created_at_unix_ms, expires_at_unix_ms) VALUES ($1, $2, $3, $4)",
            )
            .bind(&token_hash_b64)
            .bind(&user.id)
            .bind(now_i64)
            .bind(expires_i64)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        });

        Ok(AuthSession {
            token: raw_token,
            user: user.into_public(),
            expires_at_unix_ms: Some(expires_at_unix_ms),
        })
    }

    pub async fn authenticate(&self, token: &str) -> std::result::Result<AccountUser, StoreError> {
        let token_hash_b64 = hash_token_b64(token)?;
        let now_i64 = to_db_timestamp(unix_now_ms())?;

        let user = with_pool!(self, |pool| {
            sqlx::query(
                "SELECT u.id, u.username, u.display_name, u.created_at_unix_ms
                 FROM sessions s
                 JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash_b64 = $1 AND s.expires_at_unix_ms > $2",
            )
            .bind(&token_hash_b64)
            .bind(now_i64)
            .fetch_optional(pool)
            .await?
            .map(|r| -> std::result::Result<AccountUser, sqlx::Error> {
                let created: i64 = r.try_get("created_at_unix_ms")?;
                Ok(AccountUser {
                    id: r.try_get("id")?,
                    username: r.try_get("username")?,
                    display_name: r.try_get("display_name")?,
                    created_at_unix_ms: u64::try_from(created).unwrap_or_default(),
                })
            })
            .transpose()?
        });

        user.ok_or_else(|| StoreError::Unauthorized("Invalid or expired session token.".to_owned()))
    }

    pub async fn logout(&self, token: &str) -> std::result::Result<(), StoreError> {
        let token_hash_b64 = hash_token_b64(token)?;
        let now_i64 = to_db_timestamp(unix_now_ms())?;

        let deleted = with_pool!(self, |pool| {
            let mut tx = pool.begin().await?;
            let result = sqlx::query(
                "DELETE FROM sessions WHERE token_hash_b64 = $1 AND expires_at_unix_ms > $2",
            )
            .bind(&token_hash_b64)
            .bind(now_i64)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM sessions WHERE expires_at_unix_ms <= $1")
                .bind(now_i64)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            result.rows_affected()
        });

        if deleted == 0 {
            return Err(StoreError::Unauthorized(
                "Invalid or expired session token.".to_owned(),
            ));
        }
        Ok(())
    }

    pub async fn heartbeat(
        &self,
        token: &str,
        heartbeat: DeviceHeartbeat,
    ) -> std::result::Result<DeviceRecord, StoreError> {
        let device_id = heartbeat.device_id.trim().to_owned();
        if device_id.is_empty() || device_id.len() > 128 {
            return Err(StoreError::BadRequest(
                "Device ID must be between 1 and 128 characters.".to_owned(),
            ));
        }
        let name = heartbeat.name.trim().to_owned();
        if name.is_empty() || name.len() > 128 {
            return Err(StoreError::BadRequest(
                "Device name must be between 1 and 128 characters.".to_owned(),
            ));
        }
        let platform = heartbeat.platform.trim().to_owned();
        if platform.is_empty() || platform.len() > 64 {
            return Err(StoreError::BadRequest(
                "Device platform must be between 1 and 64 characters.".to_owned(),
            ));
        }

        let token_hash_b64 = hash_token_b64(token)?;
        let now_ms = unix_now_ms();
        let now_i64 = to_db_timestamp(now_ms)?;

        let host_address = heartbeat
            .host_address
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let pairing_code = heartbeat
            .pairing_code
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        let display_label = heartbeat.display_label.trim().to_owned();

        let user_id = self.active_user_id(&token_hash_b64, now_i64).await?;

        with_pool!(self, |pool| {
            sqlx::query(
                "INSERT INTO devices (user_id, device_id, name, platform, host_address, pairing_code, sharing, view_only, display_label, last_seen_unix_ms)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (user_id, device_id) DO UPDATE SET
                    name = EXCLUDED.name,
                    platform = EXCLUDED.platform,
                    host_address = EXCLUDED.host_address,
                    pairing_code = EXCLUDED.pairing_code,
                    sharing = EXCLUDED.sharing,
                    view_only = EXCLUDED.view_only,
                    display_label = EXCLUDED.display_label,
                    last_seen_unix_ms = EXCLUDED.last_seen_unix_ms",
            )
            .bind(&user_id)
            .bind(&device_id)
            .bind(&name)
            .bind(&platform)
            .bind(&host_address)
            .bind(&pairing_code)
            .bind(heartbeat.sharing)
            .bind(heartbeat.view_only)
            .bind(&display_label)
            .bind(now_i64)
            .execute(pool)
            .await?;
        });

        Ok(DeviceRecord {
            device_id,
            name,
            platform,
            host_address,
            pairing_code,
            sharing: heartbeat.sharing,
            view_only: heartbeat.view_only,
            display_label,
            last_seen_unix_ms: now_ms,
            online: true,
        })
    }

    pub async fn list_devices(
        &self,
        token: &str,
    ) -> std::result::Result<Vec<DeviceRecord>, StoreError> {
        self.list_devices_at(token, unix_now_ms()).await
    }

    pub async fn list_devices_at(
        &self,
        token: &str,
        now_ms: u64,
    ) -> std::result::Result<Vec<DeviceRecord>, StoreError> {
        let token_hash_b64 = hash_token_b64(token)?;
        let now_i64 = to_db_timestamp(now_ms)?;
        let user_id = self.active_user_id(&token_hash_b64, now_i64).await?;

        let mut devices = with_pool!(self, |pool| {
            let rows = sqlx::query(
                "SELECT device_id, name, platform, host_address, pairing_code, sharing, view_only, display_label, last_seen_unix_ms
                 FROM devices
                 WHERE user_id = $1",
            )
            .bind(&user_id)
            .fetch_all(pool)
            .await?;

            rows.into_iter()
                .map(|r| -> std::result::Result<DeviceRecord, sqlx::Error> {
                    let last_seen_i64: i64 = r.try_get("last_seen_unix_ms")?;
                    let last_seen_unix_ms = u64::try_from(last_seen_i64).unwrap_or_default();
                    Ok(DeviceRecord {
                        device_id: r.try_get("device_id")?,
                        name: r.try_get("name")?,
                        platform: r.try_get("platform")?,
                        host_address: r.try_get("host_address")?,
                        pairing_code: r.try_get("pairing_code")?,
                        sharing: r.try_get("sharing")?,
                        view_only: r.try_get("view_only")?,
                        display_label: r.try_get("display_label")?,
                        last_seen_unix_ms,
                        online: is_device_online(now_ms, last_seen_unix_ms),
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?
        });

        sort_devices(&mut devices);
        Ok(devices)
    }

    pub async fn delete_device(
        &self,
        token: &str,
        device_id: &str,
    ) -> std::result::Result<(), StoreError> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err(StoreError::BadRequest("Device ID is required.".to_owned()));
        }
        let token_hash_b64 = hash_token_b64(token)?;
        let now_i64 = to_db_timestamp(unix_now_ms())?;
        let user_id = self.active_user_id(&token_hash_b64, now_i64).await?;

        let deleted = with_pool!(self, |pool| {
            sqlx::query("DELETE FROM devices WHERE user_id = $1 AND device_id = $2")
                .bind(&user_id)
                .bind(device_id)
                .execute(pool)
                .await?
                .rows_affected()
        });

        if deleted == 0 {
            return Err(StoreError::NotFound("Device not found.".to_owned()));
        }
        Ok(())
    }

    async fn active_user_id(
        &self,
        token_hash_b64: &str,
        now_i64: i64,
    ) -> std::result::Result<String, StoreError> {
        let user_id = with_pool!(self, |pool| {
            sqlx::query_scalar::<_, String>(
                "SELECT user_id FROM sessions WHERE token_hash_b64 = $1 AND expires_at_unix_ms > $2",
            )
            .bind(token_hash_b64)
            .bind(now_i64)
            .fetch_optional(pool)
            .await?
        });

        user_id
            .ok_or_else(|| StoreError::Unauthorized("Invalid or expired session token.".to_owned()))
    }

    /// Test helper to adjust a stored device's `last_seen_unix_ms` without waiting 45 seconds.
    #[doc(hidden)]
    pub async fn set_device_last_seen_for_test(
        &self,
        username: &str,
        device_id: &str,
        last_seen_unix_ms: u64,
    ) -> Result<()> {
        let username_lower = username.trim().to_ascii_lowercase();
        let last_seen_i64 = i64::try_from(last_seen_unix_ms)
            .context("Timestamp overflows signed 64-bit integer.")?;

        let updated = with_pool!(self, |pool| {
            sqlx::query(
                "UPDATE devices
                 SET last_seen_unix_ms = $1
                 WHERE device_id = $2
                   AND user_id = (SELECT id FROM users WHERE username_lower = $3)",
            )
            .bind(last_seen_i64)
            .bind(device_id)
            .bind(&username_lower)
            .execute(pool)
            .await?
            .rows_affected()
        });

        if updated == 0 {
            anyhow::bail!("User or device not found for test timestamp update.");
        }
        Ok(())
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db_error) if db_error.is_unique_violation())
}

fn to_db_timestamp(value: u64) -> Result<i64> {
    i64::try_from(value).context("Timestamp exceeds signed 64-bit range.")
}

pub fn is_device_online(now_ms: u64, last_seen_unix_ms: u64) -> bool {
    now_ms.saturating_sub(last_seen_unix_ms) <= ONLINE_WINDOW_MS
}

pub fn sort_devices(devices: &mut [DeviceRecord]) {
    devices.sort_by(|a, b| {
        (
            Reverse(a.online && a.sharing),
            Reverse(a.online),
            a.name.to_ascii_lowercase(),
            &a.name,
            &a.device_id,
        )
            .cmp(&(
                Reverse(b.online && b.sharing),
                Reverse(b.online),
                b.name.to_ascii_lowercase(),
                &b.name,
                &b.device_id,
            ))
    });
}

pub fn validate_username(raw: &str) -> std::result::Result<&str, StoreError> {
    if !(3..=32).contains(&raw.len())
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'@'))
    {
        return Err(StoreError::BadRequest(
            "Username must be 3 to 32 characters using letters, digits, `_`, `-`, `.`, or `@`."
                .to_owned(),
        ));
    }
    Ok(raw)
}

pub fn validate_password(password: &str) -> std::result::Result<(), StoreError> {
    if password.chars().count() < 8 {
        return Err(StoreError::BadRequest(
            "Password must be at least 8 characters.".to_owned(),
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(StoreError::BadRequest(
            "Password exceeds maximum supported length.".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_display_name(raw: Option<&str>) -> std::result::Result<Option<String>, StoreError> {
    let Some(name) = raw else {
        return Ok(None);
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(StoreError::BadRequest(format!(
            "Display name must be at most {MAX_DISPLAY_NAME_CHARS} characters."
        )));
    }
    Ok(Some(trimmed.to_owned()))
}

fn hash_password(password: &str) -> Result<(String, String)> {
    let mut salt = [0u8; SALT_BYTES];
    getrandom::fill(&mut salt).map_err(|error| anyhow!("Random salt unavailable: {error}"))?;
    let mut derived = [0u8; HASH_BYTES];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, PBKDF2_ROUNDS, &mut derived);
    Ok((
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(derived),
    ))
}

fn verify_password(password: &str, salt_b64: &str, expected_hash_b64: &str) -> Result<bool> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Ok(false);
    }
    let salt = URL_SAFE_NO_PAD
        .decode(salt_b64)
        .context("Stored password salt is corrupt.")?;
    let expected = URL_SAFE_NO_PAD
        .decode(expected_hash_b64)
        .context("Stored password hash is corrupt.")?;
    if expected.len() != HASH_BYTES {
        return Ok(false);
    }
    let mut derived = [0u8; HASH_BYTES];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, PBKDF2_ROUNDS, &mut derived);
    Ok(bool::from(derived.ct_eq(&expected)))
}

fn generate_session_token() -> Result<(String, String)> {
    let mut raw = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut raw).map_err(|error| anyhow!("Session token RNG unavailable: {error}"))?;
    let token = URL_SAFE_NO_PAD.encode(raw);
    let token_hash_b64 = URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()));
    Ok((token, token_hash_b64))
}

fn hash_token_b64(token: &str) -> std::result::Result<String, StoreError> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err(StoreError::Unauthorized(
            "Missing bearer session token.".to_owned(),
        ));
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(trimmed.as_bytes())))
}

fn random_id(prefix: &str) -> Result<String> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("Random ID unavailable: {error}"))?;
    Ok(format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use remoters_core::account::DeviceRecord;

    use super::{
        PBKDF2_ROUNDS, hash_password, is_device_online, sort_devices, validate_password,
        validate_username, verify_password,
    };

    #[test]
    fn username_and_password_validation() {
        assert!(validate_username("alice_01.dev@local").is_ok());
        assert!(validate_username("ab").is_err());
        assert!(validate_username(&"a".repeat(33)).is_err());
        assert!(validate_username("bad name").is_err());
        assert!(validate_username("bad/name").is_err());

        assert!(validate_password("12345678").is_ok());
        assert!(validate_password("1234567").is_err());
    }

    #[test]
    fn password_hashing_and_constant_time_verify() {
        assert_eq!(PBKDF2_ROUNDS, 100_000);
        let (salt, hash) = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &salt, &hash).unwrap());
        assert!(!verify_password("wrong password", &salt, &hash).unwrap());
    }

    #[test]
    fn online_window_and_device_sorting() {
        assert!(is_device_online(100_000, 55_000));
        assert!(!is_device_online(100_000, 54_999));

        let mut devices = vec![
            DeviceRecord {
                device_id: "d4".into(),
                name: "Zulu-Offline".into(),
                platform: "macOS".into(),
                host_address: None,
                pairing_code: None,
                sharing: true,
                view_only: false,
                display_label: "".into(),
                last_seen_unix_ms: 1_000,
                online: false,
            },
            DeviceRecord {
                device_id: "d2".into(),
                name: "Beta-Online-Idle".into(),
                platform: "Windows".into(),
                host_address: None,
                pairing_code: None,
                sharing: false,
                view_only: false,
                display_label: "".into(),
                last_seen_unix_ms: 90_000,
                online: true,
            },
            DeviceRecord {
                device_id: "d3".into(),
                name: "Alpha-Offline".into(),
                platform: "macOS".into(),
                host_address: None,
                pairing_code: None,
                sharing: false,
                view_only: false,
                display_label: "".into(),
                last_seen_unix_ms: 1_000,
                online: false,
            },
            DeviceRecord {
                device_id: "d1".into(),
                name: "Gamma-Sharing".into(),
                platform: "macOS".into(),
                host_address: Some("192.168.1.10:47821".into()),
                pairing_code: Some("abc".into()),
                sharing: true,
                view_only: false,
                display_label: "Main display".into(),
                last_seen_unix_ms: 95_000,
                online: true,
            },
        ];
        sort_devices(&mut devices);
        let ids: Vec<_> = devices.iter().map(|d| d.device_id.as_str()).collect();
        assert_eq!(ids, vec!["d1", "d2", "d3", "d4"]);
    }
}
