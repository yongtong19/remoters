use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use remoters_core::account::{AccountClient, AccountError, AccountStorage, DeviceHeartbeat};
use remoters_server::{AccountServer, DatabaseConfig, ServerConfig, store::unix_now_ms};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "remoters-server-test-{prefix}-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn spawn_server(db_path: PathBuf) -> (AccountServer, AccountClient) {
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server = AccountServer::bind(ServerConfig::sqlite(bind, db_path))
        .await
        .unwrap();
    assert_eq!(server.store().backend_name(), "sqlite");
    let client = AccountClient::new(format!("http://{}", server.local_addr())).unwrap();
    (server, client)
}

#[tokio::test]
async fn health_endpoint_works() {
    let dir = TempDir::new("health");
    let (server, client) = spawn_server(dir.file("server.db")).await;

    let health = client.health().await.unwrap();
    assert_eq!(health.status, "ok");
    assert_eq!(health.version, env!("CARGO_PKG_VERSION"));

    server.shutdown().await;
}

#[tokio::test]
async fn sqlite_in_memory_store_works() {
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let server = AccountServer::bind(ServerConfig {
        bind,
        database: DatabaseConfig::parse(":memory:").unwrap(),
    })
    .await
    .unwrap();
    assert_eq!(server.store().backend_name(), "sqlite");

    let client = AccountClient::new(format!("http://{}", server.local_addr())).unwrap();
    let session = client
        .register("memory_user", "strong-pass-123", Some("In Memory"))
        .await
        .unwrap();
    let device = client
        .heartbeat(
            &session.token,
            &DeviceHeartbeat {
                device_id: "mem-dev".into(),
                name: "Mem-Host".into(),
                platform: "macOS".into(),
                host_address: Some("198.51.100.2:47821".into()),
                pairing_code: Some("fixture-pair".into()),
                sharing: true,
                view_only: false,
                display_label: "Built-in".into(),
            },
        )
        .await
        .unwrap();
    assert!(device.online);
    assert!(device.sharing);
    assert_eq!(client.list_devices(&session.token).await.unwrap().len(), 1);

    server.shutdown().await;
}

#[tokio::test]
async fn auth_register_duplicate_wrong_password_me_and_logout() {
    let dir = TempDir::new("auth");
    let (server, client) = spawn_server(dir.file("auth.db")).await;

    // Invalid username / short password return clean 400 errors.
    let err = client
        .register("ab", "password123", None)
        .await
        .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 400, .. })
    ));
    let err = client.register("alice", "short", None).await.unwrap_err();
    assert!(matches!(
        err.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 400, .. })
    ));

    // Register succeeds with 201 + token + user.
    let session = client
        .register("alice", "super-secret-pass", Some("Alice Mac"))
        .await
        .unwrap();
    assert_eq!(session.user.username, "alice");
    assert_eq!(session.user.display_name.as_deref(), Some("Alice Mac"));
    assert!(!session.token.is_empty());

    // Duplicate registration (including case-insensitive) returns 409.
    let dup_err = client
        .register("Alice", "another-password", None)
        .await
        .unwrap_err();
    assert!(matches!(
        dup_err.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 409, .. })
    ));

    // Wrong password returns 401.
    let wrong_pw = client.login("alice", "wrong-pass").await.unwrap_err();
    assert!(
        wrong_pw
            .downcast_ref::<AccountError>()
            .unwrap()
            .is_unauthorized()
    );

    // Unknown user returns 401.
    let unknown = client.login("nobody", "wrong-pass").await.unwrap_err();
    assert!(
        unknown
            .downcast_ref::<AccountError>()
            .unwrap()
            .is_unauthorized()
    );

    // `/me` works with valid bearer token.
    let me = client.me(&session.token).await.unwrap();
    assert_eq!(me, session.user);

    // Login produces another valid session token.
    let login_session = client.login("alice", "super-secret-pass").await.unwrap();
    assert_ne!(login_session.token, session.token);
    assert_eq!(client.me(&login_session.token).await.unwrap(), session.user);

    // Logout revokes only the logged-out token.
    client.logout(&session.token).await.unwrap();
    let revoked = client.me(&session.token).await.unwrap_err();
    assert!(
        revoked
            .downcast_ref::<AccountError>()
            .unwrap()
            .is_unauthorized()
    );
    assert_eq!(client.me(&login_session.token).await.unwrap(), session.user);

    // Second logout with revoked token fails with 401.
    let second_logout = client.logout(&session.token).await.unwrap_err();
    assert!(
        second_logout
            .downcast_ref::<AccountError>()
            .unwrap()
            .is_unauthorized()
    );

    server.shutdown().await;
}

#[tokio::test]
async fn device_heartbeat_sorting_online_offline_and_delete() {
    let dir = TempDir::new("devices");
    let (server, client) = spawn_server(dir.file("devices.db")).await;

    let alice = client
        .register("alice", "password-1234", None)
        .await
        .unwrap();
    let bob = client
        .register("bob_user", "password-1234", None)
        .await
        .unwrap();

    // Register 3 devices for Alice + 1 device for Bob.
    let d_idle = client
        .heartbeat(
            &alice.token,
            &DeviceHeartbeat {
                device_id: "dev-idle".into(),
                name: "Beta-Idle".into(),
                platform: "Windows".into(),
                host_address: None,
                pairing_code: None,
                sharing: false,
                view_only: false,
                display_label: String::new(),
            },
        )
        .await
        .unwrap();
    assert!(d_idle.online);
    assert!(!d_idle.sharing);

    let d_sharing = client
        .heartbeat(
            &alice.token,
            &DeviceHeartbeat {
                device_id: "dev-share".into(),
                name: "Zulu-Studio".into(),
                platform: "macOS".into(),
                host_address: Some("198.51.100.10:47821".into()),
                pairing_code: Some("fixture-code-1".into()),
                sharing: true,
                view_only: true,
                display_label: "Main display · 2560 × 1440".into(),
            },
        )
        .await
        .unwrap();
    assert!(d_sharing.online);
    assert!(d_sharing.sharing);
    assert!(d_sharing.view_only);

    client
        .heartbeat(
            &alice.token,
            &DeviceHeartbeat {
                device_id: "dev-offline".into(),
                name: "Alpha-Laptop".into(),
                platform: "macOS".into(),
                host_address: Some("198.51.100.11:47821".into()),
                pairing_code: Some("fixture-code-2".into()),
                sharing: true,
                view_only: false,
                display_label: "Virtual display".into(),
            },
        )
        .await
        .unwrap();

    client
        .heartbeat(
            &bob.token,
            &DeviceHeartbeat {
                device_id: "bob-dev".into(),
                name: "Bob-PC".into(),
                platform: "Windows".into(),
                host_address: None,
                pairing_code: None,
                sharing: true,
                view_only: false,
                display_label: "Primary".into(),
            },
        )
        .await
        .unwrap();

    // Mark Alpha-Laptop older than 45s so it transitions to offline.
    let stale_ms = unix_now_ms().saturating_sub(46_000);
    server
        .store()
        .set_device_last_seen_for_test("alice", "dev-offline", stale_ms)
        .await
        .unwrap();

    // Verify sorting: online+sharing (Zulu-Studio) -> online (Beta-Idle) -> offline (Alpha-Laptop).
    let list = client.list_devices(&alice.token).await.unwrap();
    let names: Vec<_> = list.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["Zulu-Studio", "Beta-Idle", "Alpha-Laptop"]);
    assert!(list[0].online && list[0].sharing);
    assert!(list[1].online && !list[1].sharing);
    assert!(!list[2].online);

    // User isolation: Bob cannot see or delete Alice's devices.
    let bob_list = client.list_devices(&bob.token).await.unwrap();
    assert_eq!(bob_list.len(), 1);
    assert_eq!(bob_list[0].device_id, "bob-dev");

    let forbidden_del = client
        .delete_device(&bob.token, "dev-share")
        .await
        .unwrap_err();
    assert!(matches!(
        forbidden_del.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 404, .. })
    ));

    // Alice deletes Beta-Idle, and deleting again returns 404.
    client
        .delete_device(&alice.token, "dev-idle")
        .await
        .unwrap();
    let list_after = client.list_devices(&alice.token).await.unwrap();
    assert_eq!(list_after.len(), 2);
    let missing_del = client
        .delete_device(&alice.token, "dev-idle")
        .await
        .unwrap_err();
    assert!(matches!(
        missing_del.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 404, .. })
    ));

    server.shutdown().await;
}

#[tokio::test]
async fn persistence_across_server_restart_and_storage_end_to_end() {
    let dir = TempDir::new("restart");
    let db_file = dir.file("server-state.db");
    let storage_file = dir.file("account.json");

    let mut storage = AccountStorage::load_from(&storage_file).unwrap();
    let initial_device_id = storage.device_id.clone();

    let (server_1, client_1) = spawn_server(db_file.clone()).await;
    storage.server_url = client_1.base_url().to_owned();

    let session = client_1
        .register("persist_user", "very-strong-password", Some("Persist"))
        .await
        .unwrap();
    storage.session = Some(session.clone());
    storage.save_to(&storage_file).unwrap();

    client_1
        .heartbeat(
            &session.token,
            &DeviceHeartbeat {
                device_id: initial_device_id.clone(),
                name: "Workstation".into(),
                platform: "macOS".into(),
                host_address: Some("100.64.0.10:47821".into()),
                pairing_code: Some("pair-token".into()),
                sharing: true,
                view_only: false,
                display_label: "Main display".into(),
            },
        )
        .await
        .unwrap();
    server_1.shutdown().await;

    // Restart server against the exact same SQLite database file.
    let (server_2, client_2) = spawn_server(db_file).await;
    let reloaded_storage = AccountStorage::load_from(&storage_file).unwrap();
    assert_eq!(reloaded_storage.device_id, initial_device_id);
    let saved_session = reloaded_storage.session.unwrap();

    // Existing bearer token still authenticates after restart.
    let me = client_2.me(&saved_session.token).await.unwrap();
    assert_eq!(me.username, "persist_user");

    // Existing user can also log in again with password.
    let fresh_login = client_2
        .login("persist_user", "very-strong-password")
        .await
        .unwrap();
    assert_eq!(fresh_login.user.id, me.id);

    // Persisted device is still returned.
    let devices = client_2.list_devices(&fresh_login.token).await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id, initial_device_id);
    assert_eq!(
        devices[0].host_address.as_deref(),
        Some("100.64.0.10:47821")
    );

    server_2.shutdown().await;
}

#[tokio::test]
async fn postgres_end_to_end_when_configured() {
    let Ok(pg_url) = std::env::var("TEST_POSTGRES_URL") else {
        return;
    };
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let database = DatabaseConfig::parse(&pg_url).unwrap();
    let server = AccountServer::bind(ServerConfig {
        bind,
        database: database.clone(),
    })
    .await
    .unwrap();
    assert_eq!(server.store().backend_name(), "postgres");

    let client = AccountClient::new(format!("http://{}", server.local_addr())).unwrap();
    let username = format!(
        "pg_{}_{}",
        unix_now_ms(),
        TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let session = client
        .register(&username, "postgres-pass-123", Some("Postgres User"))
        .await
        .unwrap();
    let dup = client
        .register(&username, "postgres-pass-123", None)
        .await
        .unwrap_err();
    assert!(matches!(
        dup.downcast_ref::<AccountError>(),
        Some(AccountError::Api { status: 409, .. })
    ));

    client
        .heartbeat(
            &session.token,
            &DeviceHeartbeat {
                device_id: "pg-dev-1".into(),
                name: "PG-Node".into(),
                platform: "Linux".into(),
                host_address: Some("198.51.100.55:47821".into()),
                pairing_code: Some("pg-code".into()),
                sharing: true,
                view_only: false,
                display_label: "Primary".into(),
            },
        )
        .await
        .unwrap();
    let list = client.list_devices(&session.token).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].device_id, "pg-dev-1");

    client
        .delete_device(&session.token, "pg-dev-1")
        .await
        .unwrap();
    assert!(
        client
            .list_devices(&session.token)
            .await
            .unwrap()
            .is_empty()
    );

    client.logout(&session.token).await.unwrap();
    server.shutdown().await;
}
