use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::Duration,
};

use remoters_core::{
    account::{
        AccountClient, AccountError, AccountStorage, AccountUser, DEFAULT_SERVER_URL,
        DeviceHeartbeat, DeviceRecord,
    },
    network::{self, discovery::local_hostname},
};
use tokio::{
    runtime::Builder,
    sync::{Notify, mpsc, oneshot},
    time::{self, MissedTickBehavior},
};

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
pub const DEVICES_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
pub const ACCOUNT_SERVER_ENV: &str = "REMOTERS_ACCOUNT_SERVER";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LocalSharingSnapshot {
    pub sharing: bool,
    pub bind_address: Option<SocketAddrV4>,
    pub pairing_code: Option<String>,
    pub view_only: bool,
    pub display_label: String,
}

impl LocalSharingSnapshot {
    pub fn to_heartbeat(&self, device_id: &str) -> DeviceHeartbeat {
        DeviceHeartbeat {
            device_id: device_id.to_owned(),
            name: local_hostname(),
            platform: host_platform().to_owned(),
            host_address: resolve_heartbeat_address(if self.sharing {
                self.bind_address
            } else {
                None
            }),
            pairing_code: if self.sharing {
                self.pairing_code.clone()
            } else {
                None
            },
            sharing: self.sharing,
            view_only: self.sharing && self.view_only,
            display_label: if self.sharing {
                self.display_label.clone()
            } else {
                String::new()
            },
        }
    }
}

#[derive(Debug)]
pub enum AccountEvent {
    AuthBusy(bool),
    SignedIn {
        server_url: String,
        user: AccountUser,
    },
    SignedOut {
        message: Option<String>,
        is_error: bool,
    },
    AuthError(String),
    DevicesUpdated(Vec<DeviceRecord>),
    DeviceRemoved(String),
    ActionError(String),
}

enum AccountCommand {
    SignIn {
        server_url: String,
        username: String,
        password: String,
    },
    Register {
        server_url: String,
        username: String,
        password: String,
        display_name: Option<String>,
    },
    SignOut,
    RefreshDevices,
    DeleteDevice(String),
    SendHeartbeatNow,
}

pub struct AccountRuntime {
    device_id: String,
    initial_server_url: String,
    initial_user: Option<AccountUser>,
    pub events: Receiver<AccountEvent>,
    commands: mpsc::UnboundedSender<AccountCommand>,
    sharing_state: Arc<Mutex<LocalSharingSnapshot>>,
    _stop: oneshot::Sender<()>,
}

impl AccountRuntime {
    pub fn start(cli_server_override: Option<String>, wake: Arc<Notify>) -> Self {
        let mut storage = match AccountStorage::load() {
            Ok(storage) => storage,
            Err(_) => AccountStorage {
                server_url: DEFAULT_SERVER_URL.to_owned(),
                device_id: AccountStorage::generate_device_id()
                    .unwrap_or_else(|_| format!("desktop-{}", std::process::id())),
                session: None,
            },
        };
        let env_override = env_account_server();
        let initial_server_url = resolve_server_url(
            cli_server_override.as_deref(),
            env_override.as_deref(),
            &storage.server_url,
        );
        storage.server_url = initial_server_url.clone();
        let device_id = storage.device_id.clone();
        let initial_user = storage.session.as_ref().map(|session| session.user.clone());

        let (events_tx, events) = channel();
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let sharing_state = Arc::new(Mutex::new(LocalSharingSnapshot::default()));
        let worker_sharing = sharing_state.clone();

        let spawn_result = thread::Builder::new()
            .name("account-runtime".into())
            .spawn(move || {
                let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                    return;
                };
                runtime.block_on(run_account_loop(
                    storage,
                    worker_sharing,
                    events_tx,
                    commands_rx,
                    stop_rx,
                    wake,
                ));
            });
        if let Err(error) = spawn_result {
            eprintln!("Failed to spawn account runtime thread: {error}");
        }

        Self {
            device_id,
            initial_server_url,
            initial_user,
            events,
            commands: commands_tx,
            sharing_state,
            _stop: stop_tx,
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn initial_server_url(&self) -> &str {
        &self.initial_server_url
    }

    pub fn initial_user(&self) -> Option<&AccountUser> {
        self.initial_user.as_ref()
    }

    pub fn sign_in(&self, server_url: String, username: String, password: String) {
        let _ = self.commands.send(AccountCommand::SignIn {
            server_url,
            username,
            password,
        });
    }

    pub fn register(
        &self,
        server_url: String,
        username: String,
        password: String,
        display_name: Option<String>,
    ) {
        let _ = self.commands.send(AccountCommand::Register {
            server_url,
            username,
            password,
            display_name,
        });
    }

    pub fn sign_out(&self) {
        let _ = self.commands.send(AccountCommand::SignOut);
    }

    pub fn refresh_devices(&self) {
        let _ = self.commands.send(AccountCommand::RefreshDevices);
    }

    pub fn delete_device(&self, device_id: String) {
        let _ = self.commands.send(AccountCommand::DeleteDevice(device_id));
    }

    pub fn update_sharing(&self, snapshot: LocalSharingSnapshot) {
        let changed = {
            let mut guard = self
                .sharing_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *guard == snapshot {
                false
            } else {
                *guard = snapshot;
                true
            }
        };
        if changed {
            let _ = self.commands.send(AccountCommand::SendHeartbeatNow);
        }
    }
}

async fn run_account_loop(
    mut storage: AccountStorage,
    sharing_state: Arc<Mutex<LocalSharingSnapshot>>,
    events_tx: Sender<AccountEvent>,
    mut commands: mpsc::UnboundedReceiver<AccountCommand>,
    mut stop: oneshot::Receiver<()>,
    wake: Arc<Notify>,
) {
    let mut heartbeat_tick = time::interval(HEARTBEAT_INTERVAL);
    heartbeat_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut refresh_tick = time::interval(DEVICES_REFRESH_INTERVAL);
    refresh_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Consume the immediate initial tick so we run startup sync explicitly below.
    heartbeat_tick.tick().await;
    refresh_tick.tick().await;

    if storage.session.is_some() {
        sync_heartbeat_and_devices(&mut storage, &sharing_state, &events_tx, &wake, true).await;
    }

    loop {
        tokio::select! {
            _ = &mut stop => break,
            Some(command) = commands.recv() => {
                handle_command(
                    command,
                    &mut storage,
                    &sharing_state,
                    &events_tx,
                    &wake,
                )
                .await;
            }
            _ = heartbeat_tick.tick(), if storage.session.is_some() => {
                send_heartbeat(&mut storage, &sharing_state, &events_tx, &wake).await;
            }
            _ = refresh_tick.tick(), if storage.session.is_some() => {
                refresh_devices(&mut storage, &events_tx, &wake).await;
            }
        }
    }
}

async fn handle_command(
    command: AccountCommand,
    storage: &mut AccountStorage,
    sharing_state: &Arc<Mutex<LocalSharingSnapshot>>,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
) {
    match command {
        AccountCommand::SignIn {
            server_url,
            username,
            password,
        } => {
            authenticate(
                storage,
                sharing_state,
                events_tx,
                wake,
                server_url,
                AuthRequest::SignIn { username, password },
            )
            .await;
        }
        AccountCommand::Register {
            server_url,
            username,
            password,
            display_name,
        } => {
            authenticate(
                storage,
                sharing_state,
                events_tx,
                wake,
                server_url,
                AuthRequest::Register {
                    username,
                    password,
                    display_name,
                },
            )
            .await;
        }
        AccountCommand::SignOut => {
            if let Some(session) = storage.session.take() {
                if let Ok(client) = AccountClient::new(&storage.server_url) {
                    let _ = client.logout(&session.token).await;
                }
                let _ = storage.save();
            }
            emit(
                events_tx,
                wake,
                AccountEvent::SignedOut {
                    message: Some("Signed out.".to_owned()),
                    is_error: false,
                },
            );
        }
        AccountCommand::RefreshDevices => {
            if storage.session.is_some() {
                refresh_devices(storage, events_tx, wake).await;
            }
        }
        AccountCommand::DeleteDevice(device_id) => {
            let Some(token) = storage.session.as_ref().map(|s| s.token.clone()) else {
                return;
            };
            let Ok(client) = AccountClient::new(&storage.server_url) else {
                emit(
                    events_tx,
                    wake,
                    AccountEvent::ActionError("Invalid account server URL.".to_owned()),
                );
                return;
            };
            match client.delete_device(&token, &device_id).await {
                Ok(()) => {
                    emit(events_tx, wake, AccountEvent::DeviceRemoved(device_id));
                    refresh_devices(storage, events_tx, wake).await;
                }
                Err(error) => {
                    if is_unauthorized_error(&error) {
                        expire_session(storage, events_tx, wake);
                    } else {
                        emit(
                            events_tx,
                            wake,
                            AccountEvent::ActionError(format!("{error:#}")),
                        );
                    }
                }
            }
        }
        AccountCommand::SendHeartbeatNow => {
            if storage.session.is_some() {
                sync_heartbeat_and_devices(storage, sharing_state, events_tx, wake, false).await;
            }
        }
    }
}

enum AuthRequest {
    SignIn {
        username: String,
        password: String,
    },
    Register {
        username: String,
        password: String,
        display_name: Option<String>,
    },
}

async fn authenticate(
    storage: &mut AccountStorage,
    sharing_state: &Arc<Mutex<LocalSharingSnapshot>>,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
    server_url: String,
    request: AuthRequest,
) {
    emit(events_tx, wake, AccountEvent::AuthBusy(true));
    let client = match AccountClient::new(&server_url) {
        Ok(client) => client,
        Err(error) => {
            emit(events_tx, wake, AccountEvent::AuthBusy(false));
            emit(
                events_tx,
                wake,
                AccountEvent::AuthError(format!("{error:#}")),
            );
            return;
        }
    };
    let result = match request {
        AuthRequest::SignIn { username, password } => client.login(&username, &password).await,
        AuthRequest::Register {
            username,
            password,
            display_name,
        } => {
            client
                .register(&username, &password, display_name.as_deref())
                .await
        }
    };
    match result {
        Ok(session) => {
            storage.server_url = client.base_url().to_owned();
            storage.session = Some(session.clone());
            if let Err(error) = storage.save() {
                emit(
                    events_tx,
                    wake,
                    AccountEvent::ActionError(format!(
                        "Signed in, but could not save local credentials: {error:#}"
                    )),
                );
            }
            emit(events_tx, wake, AccountEvent::AuthBusy(false));
            emit(
                events_tx,
                wake,
                AccountEvent::SignedIn {
                    server_url: storage.server_url.clone(),
                    user: session.user,
                },
            );
            sync_heartbeat_and_devices(storage, sharing_state, events_tx, wake, false).await;
        }
        Err(error) => {
            emit(events_tx, wake, AccountEvent::AuthBusy(false));
            emit(
                events_tx,
                wake,
                AccountEvent::AuthError(format!("{error:#}")),
            );
        }
    }
}

async fn sync_heartbeat_and_devices(
    storage: &mut AccountStorage,
    sharing_state: &Arc<Mutex<LocalSharingSnapshot>>,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
    validate_session: bool,
) {
    let Some(token) = storage.session.as_ref().map(|s| s.token.clone()) else {
        return;
    };
    let Ok(client) = AccountClient::new(&storage.server_url) else {
        return;
    };
    if validate_session {
        match client.me(&token).await {
            Ok(user) => {
                if let Some(session) = storage.session.as_mut()
                    && session.user != user
                {
                    session.user = user.clone();
                    let _ = storage.save();
                }
                emit(
                    events_tx,
                    wake,
                    AccountEvent::SignedIn {
                        server_url: storage.server_url.clone(),
                        user,
                    },
                );
            }
            Err(error) if is_unauthorized_error(&error) => {
                expire_session(storage, events_tx, wake);
                return;
            }
            Err(_) => {}
        }
    }

    let heartbeat = current_heartbeat(sharing_state, &storage.device_id);
    if let Err(error) = client.heartbeat(&token, &heartbeat).await
        && is_unauthorized_error(&error)
    {
        expire_session(storage, events_tx, wake);
        return;
    }

    match client.list_devices(&token).await {
        Ok(devices) => emit(events_tx, wake, AccountEvent::DevicesUpdated(devices)),
        Err(error) if is_unauthorized_error(&error) => {
            expire_session(storage, events_tx, wake);
        }
        Err(_) => {}
    }
}

async fn send_heartbeat(
    storage: &mut AccountStorage,
    sharing_state: &Arc<Mutex<LocalSharingSnapshot>>,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
) {
    let Some(token) = storage.session.as_ref().map(|s| s.token.clone()) else {
        return;
    };
    let Ok(client) = AccountClient::new(&storage.server_url) else {
        return;
    };
    let heartbeat = current_heartbeat(sharing_state, &storage.device_id);
    if let Err(error) = client.heartbeat(&token, &heartbeat).await
        && is_unauthorized_error(&error)
    {
        expire_session(storage, events_tx, wake);
    }
}

async fn refresh_devices(
    storage: &mut AccountStorage,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
) {
    let Some(token) = storage.session.as_ref().map(|s| s.token.clone()) else {
        return;
    };
    let Ok(client) = AccountClient::new(&storage.server_url) else {
        return;
    };
    match client.list_devices(&token).await {
        Ok(devices) => emit(events_tx, wake, AccountEvent::DevicesUpdated(devices)),
        Err(error) if is_unauthorized_error(&error) => {
            expire_session(storage, events_tx, wake);
        }
        Err(_) => {}
    }
}

fn current_heartbeat(
    sharing_state: &Arc<Mutex<LocalSharingSnapshot>>,
    device_id: &str,
) -> DeviceHeartbeat {
    let snapshot = sharing_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    snapshot.to_heartbeat(device_id)
}

fn expire_session(
    storage: &mut AccountStorage,
    events_tx: &Sender<AccountEvent>,
    wake: &Arc<Notify>,
) {
    storage.clear_session();
    let _ = storage.save();
    emit(
        events_tx,
        wake,
        AccountEvent::SignedOut {
            message: Some("Session expired. Please sign in again.".to_owned()),
            is_error: true,
        },
    );
}

fn emit(events_tx: &Sender<AccountEvent>, wake: &Arc<Notify>, event: AccountEvent) {
    let _ = events_tx.send(event);
    wake.notify_one();
}

fn is_unauthorized_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<AccountError>()
        .is_some_and(AccountError::is_unauthorized)
}

pub fn env_account_server() -> Option<String> {
    std::env::var(ACCOUNT_SERVER_ENV)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub fn resolve_server_url(
    cli_override: Option<&str>,
    env_override: Option<&str>,
    stored: &str,
) -> String {
    for candidate in [cli_override, env_override, Some(stored)] {
        if let Some(value) = candidate.map(str::trim).filter(|value| !value.is_empty()) {
            return value.trim_end_matches('/').to_owned();
        }
    }
    DEFAULT_SERVER_URL.to_owned()
}

pub fn host_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Desktop"
    }
}

pub fn best_local_ipv4() -> Option<Ipv4Addr> {
    let mut candidates: Vec<(u8, Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) if network::is_allowed_ip(v4.ip) => {
                let rank = if v4.ip.is_loopback() {
                    3
                } else if v4.ip.is_link_local() {
                    2
                } else if v4.ip.is_private() {
                    0
                } else {
                    // CGNAT (Tailscale 100.64/10) or other VPN/reachable IPv4
                    1
                };
                Some((rank, v4.ip))
            }
            _ => None,
        })
        .collect();
    candidates.sort_by_key(|(rank, ip)| (*rank, *ip));
    candidates.into_iter().next().map(|(_, ip)| ip)
}

pub fn resolve_heartbeat_address(sharing_addr: Option<SocketAddrV4>) -> Option<String> {
    match sharing_addr {
        Some(addr) if addr.ip().is_unspecified() => {
            let ip = best_local_ipv4().unwrap_or(Ipv4Addr::LOCALHOST);
            Some(SocketAddrV4::new(ip, addr.port()).to_string())
        }
        Some(addr) => Some(addr.to_string()),
        None => best_local_ipv4().map(|ip| ip.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::{LocalSharingSnapshot, resolve_heartbeat_address, resolve_server_url};

    #[test]
    fn resolve_server_url_prefers_cli_then_env_then_stored_then_default() {
        assert_eq!(
            resolve_server_url(
                Some(" http://10.0.0.5:8787/ "),
                Some("http://env:8787"),
                "http://stored:8787"
            ),
            "http://10.0.0.5:8787"
        );
        assert_eq!(
            resolve_server_url(None, Some("http://env:8787/"), "http://stored:8787"),
            "http://env:8787"
        );
        assert_eq!(
            resolve_server_url(None, None, "http://stored:8787/"),
            "http://stored:8787"
        );
        assert_eq!(
            resolve_server_url(Some("  "), None, ""),
            "http://127.0.0.1:8787"
        );
    }

    #[test]
    fn resolve_heartbeat_address_preserves_explicit_socket_addr() {
        let addr = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 42), 47821);
        assert_eq!(
            resolve_heartbeat_address(Some(addr)),
            Some("192.168.1.42:47821".to_owned())
        );
    }

    #[test]
    fn resolve_heartbeat_address_replaces_unspecified_ip() {
        let addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 47821);
        let resolved = resolve_heartbeat_address(Some(addr)).expect("resolved address");
        assert!(resolved.ends_with(":47821"));
        assert!(!resolved.starts_with("0.0.0.0:"));
    }

    #[test]
    fn sharing_snapshot_builds_heartbeat_cleanly() {
        let idle = LocalSharingSnapshot::default().to_heartbeat("dev-1");
        assert_eq!(idle.device_id, "dev-1");
        assert!(!idle.sharing);
        assert!(!idle.view_only);
        assert_eq!(idle.pairing_code, None);
        assert!(idle.display_label.is_empty());

        let sharing = LocalSharingSnapshot {
            sharing: true,
            bind_address: Some(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 8), 47821)),
            pairing_code: Some("ABCD-1234".to_owned()),
            view_only: true,
            display_label: "Virtual display · Match viewer".to_owned(),
        }
        .to_heartbeat("dev-1");
        assert!(sharing.sharing);
        assert!(sharing.view_only);
        assert_eq!(sharing.host_address.as_deref(), Some("10.0.0.8:47821"));
        assert_eq!(sharing.pairing_code.as_deref(), Some("ABCD-1234"));
        assert_eq!(sharing.display_label, "Virtual display · Match viewer");
    }
}
