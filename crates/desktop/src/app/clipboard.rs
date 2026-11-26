use std::time::Instant;

use gpui::{App, Context};

use crate::{clipboard, i18n};

use super::state::Desktop;

impl Desktop {
    pub(crate) fn sync_clipboard(&mut self, cx: &App) {
        let can_send_viewer = self
            .active_session()
            .is_some_and(|tab| tab.closed_notice.is_none() && tab.viewer.can_control);
        let can_send_host = !self.view_only
            && self
                .sharing
                .as_ref()
                .is_some_and(|sharing| !sharing.peers.is_empty());
        if !can_send_viewer && !can_send_host {
            return;
        }
        let Some((hash, data)) = clipboard::read_clipboard(cx) else {
            return;
        };
        if self.last_clipboard_hash == Some(hash) {
            return;
        }
        self.last_clipboard_hash = Some(hash);
        if let Some(tab) = self.active_session()
            && tab.closed_notice.is_none()
            && tab.viewer.can_control
        {
            tab.viewer.connection.send_clipboard(data);
        } else if let Some(sharing) = &self.sharing
            && !sharing.peers.is_empty()
            && !self.view_only
        {
            sharing.host.send_clipboard(data);
        }
    }

    pub(crate) fn drop_files(&mut self, paths: &gpui::ExternalPaths, cx: &mut Context<Self>) {
        let Some(tab) = self.active_session_mut() else {
            return;
        };
        if tab.closed_notice.is_some() {
            return;
        }
        if !tab.viewer.can_control {
            tab.viewer.transfer_notice = Some((
                i18n::tr().view_only_file_drop_disabled.into(),
                true,
                false,
                Instant::now(),
            ));
            cx.notify();
            return;
        }
        match clipboard::collect_drop_entries(paths.paths()) {
            Ok(entries) => {
                let count = entries.len();
                tab.viewer.transfer_notice =
                    Some((i18n::fmt_sending_files(count), false, true, Instant::now()));
                tab.viewer.connection.send_files(entries);
            }
            Err(error) => {
                tab.viewer.transfer_notice =
                    Some((format!("{error:#}"), true, false, Instant::now()));
            }
        }
        cx.notify();
    }
}
