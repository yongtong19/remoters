use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
pub const DEFAULT_DATABASE_URL: &str = "./remoters-server.db";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatabaseConfig {
    Sqlite { path: PathBuf, in_memory: bool },
    Postgres { url: String },
}

impl DatabaseConfig {
    /// Parses a SQLite filesystem path, `sqlite:` URI, `:memory:`, or PostgreSQL connection URL.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("Database path or URL cannot be empty.");
        }

        if matches!(
            trimmed,
            ":memory:" | "sqlite::memory:" | "sqlite://:memory:"
        ) {
            return Ok(Self::Sqlite {
                path: PathBuf::from(":memory:"),
                in_memory: true,
            });
        }

        if let Some(rest) = trimmed
            .strip_prefix("sqlite://")
            .or_else(|| trimmed.strip_prefix("sqlite:"))
        {
            if rest.is_empty() {
                bail!("SQLite URL `{trimmed}` is missing a database path.");
            }
            if rest == ":memory:" {
                return Ok(Self::Sqlite {
                    path: PathBuf::from(":memory:"),
                    in_memory: true,
                });
            }
            return Ok(Self::Sqlite {
                path: PathBuf::from(rest),
                in_memory: false,
            });
        }

        if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
            return Ok(Self::Postgres {
                url: trimmed.to_owned(),
            });
        }

        if trimmed.contains("://") {
            bail!(
                "Unsupported database URL `{trimmed}`. Use a SQLite path/`sqlite://` or PostgreSQL `postgres://`/`postgresql://`."
            );
        }

        Ok(Self::Sqlite {
            path: PathBuf::from(trimmed),
            in_memory: false,
        })
    }

    /// Formats the database target for logs with any embedded password redacted.
    pub fn redacted_display(&self) -> String {
        match self {
            Self::Sqlite {
                in_memory: true, ..
            } => "sqlite::memory:".to_owned(),
            Self::Sqlite { path, .. } => format!("sqlite:{}", path.display()),
            Self::Postgres { url } => redact_url_password(url),
        }
    }
}

fn redact_url_password(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];
    let Some(at_offset) = authority.rfind('@') else {
        return url.to_owned();
    };
    let userinfo = &authority[..at_offset];
    let Some(colon_offset) = userinfo.find(':') else {
        return url.to_owned();
    };

    let prefix = &url[..authority_start + colon_offset + 1];
    let suffix = &url[authority_start + at_offset..];
    format!("{prefix}***{suffix}")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub database: DatabaseConfig,
}

impl ServerConfig {
    pub fn sqlite(bind: SocketAddr, path: impl Into<PathBuf>) -> Self {
        Self {
            bind,
            database: DatabaseConfig::Sqlite {
                path: path.into(),
                in_memory: false,
            },
        }
    }

    /// Builds server configuration from environment variables and CLI arguments.
    pub fn from_env_and_args<I, S>(args: I) -> Result<Option<Self>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut bind_raw =
            non_empty_env("REMOTERS_SERVER_BIND").unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let mut db_raw = non_empty_env("REMOTERS_DATABASE_URL")
            .or_else(|| non_empty_env("DATABASE_URL"))
            .or_else(|| non_empty_env("REMOTERS_SERVER_DATA"))
            .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned());

        let mut iter = args.into_iter().skip(1);
        while let Some(arg) = iter.next() {
            let arg = arg.as_ref();
            if arg == "-h" || arg == "--help" {
                return Ok(None);
            } else if arg == "--bind" {
                let value = iter
                    .next()
                    .context("Expected a socket address after `--bind`.")?;
                bind_raw = value.as_ref().to_owned();
            } else if let Some(value) = arg.strip_prefix("--bind=") {
                bind_raw = value.to_owned();
            } else if arg == "--data" || arg == "--db" || arg == "--database-url" {
                let value = iter
                    .next()
                    .with_context(|| format!("Expected a database path or URL after `{arg}`."))?;
                db_raw = value.as_ref().to_owned();
            } else if let Some(value) = arg
                .strip_prefix("--data=")
                .or_else(|| arg.strip_prefix("--db="))
                .or_else(|| arg.strip_prefix("--database-url="))
            {
                db_raw = value.to_owned();
            } else {
                bail!(
                    "Unknown argument `{arg}`. Use `--bind <addr>` and `--db <path-or-url>` (or `--database-url` / `--data`)."
                );
            }
        }

        let bind: SocketAddr = bind_raw.trim().parse().with_context(|| {
            format!("Invalid bind address `{bind_raw}` (expected e.g. `127.0.0.1:8787`).")
        })?;
        let database = DatabaseConfig::parse(&db_raw)?;

        Ok(Some(Self { bind, database }))
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf};

    use super::{DatabaseConfig, ServerConfig};

    #[test]
    fn parses_cli_flags_and_database_targets() {
        let cfg = ServerConfig::from_env_and_args([
            "remoters-server",
            "--bind",
            "127.0.0.1:9999",
            "--db=/tmp/test-data.db",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:9999".parse::<SocketAddr>().unwrap());
        assert_eq!(
            cfg.database,
            DatabaseConfig::Sqlite {
                path: PathBuf::from("/tmp/test-data.db"),
                in_memory: false
            }
        );
        assert_eq!(cfg.database.redacted_display(), "sqlite:/tmp/test-data.db");

        let pg = ServerConfig::from_env_and_args([
            "remoters-server",
            "--database-url",
            "postgres://remoters:super-secret@db.internal:5432/remoters?sslmode=require",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            pg.database,
            DatabaseConfig::Postgres {
                url: "postgres://remoters:super-secret@db.internal:5432/remoters?sslmode=require"
                    .to_owned()
            }
        );
        assert_eq!(
            pg.database.redacted_display(),
            "postgres://remoters:***@db.internal:5432/remoters?sslmode=require"
        );
    }

    #[test]
    fn parses_sqlite_uris_memory_and_rejects_unknown_schemes() {
        assert_eq!(
            DatabaseConfig::parse(":memory:").unwrap(),
            DatabaseConfig::Sqlite {
                path: PathBuf::from(":memory:"),
                in_memory: true,
            }
        );
        assert_eq!(
            DatabaseConfig::parse("sqlite://:memory:").unwrap(),
            DatabaseConfig::Sqlite {
                path: PathBuf::from(":memory:"),
                in_memory: true,
            }
        );
        assert_eq!(
            DatabaseConfig::parse("sqlite:///data/remoters.db").unwrap(),
            DatabaseConfig::Sqlite {
                path: PathBuf::from("/data/remoters.db"),
                in_memory: false,
            }
        );
        assert_eq!(
            DatabaseConfig::parse("postgresql://user@localhost/remoters")
                .unwrap()
                .redacted_display(),
            "postgresql://user@localhost/remoters"
        );
        assert!(DatabaseConfig::parse("mysql://user:pass@localhost/db").is_err());
        assert!(DatabaseConfig::parse("sqlite://").is_err());
        assert!(DatabaseConfig::parse("   ").is_err());
    }

    #[test]
    fn help_flag_returns_none() {
        assert!(
            ServerConfig::from_env_and_args(["remoters-server", "--help"])
                .unwrap()
                .is_none()
        );
    }
}
