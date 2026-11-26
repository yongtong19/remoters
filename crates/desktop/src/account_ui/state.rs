use std::net::SocketAddrV4;

use gpui::{Context, Entity, Window, prelude::*};
use gpui_component::input::InputState;
use remoters_core::{
    account::{AccountUser, DeviceRecord},
    network::Pairing,
};

use crate::{
    Desktop,
    account::{AccountEvent, AccountRuntime, LocalSharingSnapshot},
};

use super::helpers::{
    next_selection_after_removal, reconcile_selection_with_devices, resolve_selected_device,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AuthMode {
    #[default]
    SignIn,
    Register,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum AccountSelection {
    #[default]
    None,
    Auth,
    Device(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceStatusKind {
    Sharing,
    Online,
    Offline,
}

pub struct AccountConnectTarget {
    pub address: SocketAddrV4,
    pub pairing: Pairing,
}

pub struct AccountState {
    pub runtime: AccountRuntime,
    pub user: Option<AccountUser>,
    pub devices: Vec<DeviceRecord>,
    pub selection: AccountSelection,
    pub auth_mode: AuthMode,
    pub auth_busy: bool,
    pub auth_error: Option<String>,
    pub show_password: bool,
    pub server_url: Entity<InputState>,
    pub username: Entity<InputState>,
    pub password: Entity<InputState>,
    pub display_name: Entity<InputState>,
}

impl AccountState {
    pub fn new(
        window: &mut Window,
        cli_server_override: Option<String>,
        wake: std::sync::Arc<tokio::sync::Notify>,
        cx: &mut Context<Desktop>,
    ) -> Self {
        let runtime = AccountRuntime::start(cli_server_override, wake);
        let initial_server = runtime.initial_server_url().to_owned();
        let initial_user = runtime.initial_user().cloned();

        let s = crate::i18n::tr();
        let server_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://127.0.0.1:8787")
                .default_value(initial_server)
        });
        let username = cx.new(|cx| InputState::new(window, cx).placeholder(s.username_placeholder));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(s.password)
                .masked(true)
        });
        let display_name =
            cx.new(|cx| InputState::new(window, cx).placeholder(s.display_name_placeholder));

        Self {
            runtime,
            user: initial_user,
            devices: Vec::new(),
            selection: AccountSelection::None,
            auth_mode: AuthMode::SignIn,
            auth_busy: false,
            auth_error: None,
            show_password: false,
            server_url,
            username,
            password,
            display_name,
        }
    }

    pub fn is_active_workspace(&self) -> bool {
        !matches!(self.selection, AccountSelection::None)
    }

    pub fn selected_device(&self) -> Option<&DeviceRecord> {
        resolve_selected_device(&self.devices, &self.selection)
    }

    pub fn local_device_id(&self) -> &str {
        self.runtime.device_id()
    }

    pub fn sync_sharing(&self, snapshot: LocalSharingSnapshot) {
        self.runtime.update_sharing(snapshot);
    }

    pub fn open_auth(&mut self, window: &mut Window, cx: &mut Context<Desktop>) {
        self.selection = AccountSelection::Auth;
        self.auth_error = None;
        self.username
            .update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn poll(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> (bool, Option<(String, bool)>) {
        let mut changed = false;
        let mut status_notice = None;
        for event in self.runtime.events.try_iter() {
            changed = true;
            match event {
                AccountEvent::AuthBusy(busy) => {
                    self.auth_busy = busy;
                }
                AccountEvent::SignedIn { server_url, user } => {
                    self.user = Some(user);
                    self.auth_busy = false;
                    self.auth_error = None;
                    self.password
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    self.server_url
                        .update(cx, |input, cx| input.set_value(server_url, window, cx));
                    if matches!(self.selection, AccountSelection::Auth) {
                        self.selection = AccountSelection::None;
                    }
                }
                AccountEvent::SignedOut { message, is_error } => {
                    self.user = None;
                    self.devices.clear();
                    self.auth_busy = false;
                    self.auth_error = None;
                    if matches!(self.selection, AccountSelection::Device(_)) {
                        self.selection = AccountSelection::None;
                    }
                    if let Some(message) = message {
                        status_notice = Some((message, is_error));
                    }
                }
                AccountEvent::AuthError(error) => {
                    self.auth_busy = false;
                    self.auth_error = Some(error);
                }
                AccountEvent::DevicesUpdated(devices) => {
                    self.selection = reconcile_selection_with_devices(&self.selection, &devices);
                    self.devices = devices;
                }
                AccountEvent::DeviceRemoved(device_id) => {
                    self.devices.retain(|device| device.device_id != device_id);
                    self.selection = next_selection_after_removal(&self.selection, &device_id);
                    status_notice = Some((crate::i18n::tr().device_removed.to_owned(), false));
                }
                AccountEvent::ActionError(error) => {
                    status_notice = Some((error, true));
                }
            }
        }
        (changed, status_notice)
    }

    pub(super) fn submit_auth(&mut self, cx: &mut Context<Desktop>) {
        if self.auth_busy {
            return;
        }
        let s = crate::i18n::tr();
        let server_url = self.server_url.read(cx).value().trim().to_owned();
        let username = self.username.read(cx).value().trim().to_owned();
        let password = self.password.read(cx).value().to_string();
        if server_url.is_empty() {
            self.auth_error = Some(s.err_enter_server_url.to_owned());
            cx.notify();
            return;
        }
        if username.is_empty() {
            self.auth_error = Some(s.err_enter_username.to_owned());
            cx.notify();
            return;
        }
        if password.is_empty() {
            self.auth_error = Some(s.err_enter_password.to_owned());
            cx.notify();
            return;
        }
        self.auth_error = None;
        self.auth_busy = true;
        match self.auth_mode {
            AuthMode::SignIn => self.runtime.sign_in(server_url, username, password),
            AuthMode::Register => {
                let raw_display = self.display_name.read(cx).value().trim().to_owned();
                let display_name = (!raw_display.is_empty()).then_some(raw_display);
                self.runtime
                    .register(server_url, username, password, display_name);
            }
        }
        cx.notify();
    }
}
