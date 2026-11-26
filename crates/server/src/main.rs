use anyhow::Result;
use remoters_server::{AccountServer, ServerConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let Some(config) = ServerConfig::from_env_and_args(std::env::args())? else {
        println!(
            "remoters-server {}\n\nUSAGE:\n  remoters-server [--bind <addr>] [--db <path-or-url>]\n\nOPTIONS:\n  --bind <addr>          Listen socket (default: {})\n  --db <path-or-url>     SQLite file/`:memory:` or PostgreSQL URL (aliases: --database-url, --data)\n\nENV:\n  REMOTERS_SERVER_BIND   Default: {}\n  REMOTERS_DATABASE_URL  Default: {} (also checks DATABASE_URL, REMOTERS_SERVER_DATA)",
            env!("CARGO_PKG_VERSION"),
            remoters_server::DEFAULT_BIND,
            remoters_server::DEFAULT_BIND,
            remoters_server::DEFAULT_DATABASE_URL,
        );
        return Ok(());
    };

    let db_display = config.database.redacted_display();
    let server = AccountServer::bind(config).await?;
    println!(
        "remoters-server listening on http://{} (db: {}, backend: {})",
        server.local_addr(),
        db_display,
        server.store().backend_name(),
    );

    wait_for_shutdown_signal().await;
    server.shutdown().await;
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(ref mut term) = sigterm {
                    term.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
