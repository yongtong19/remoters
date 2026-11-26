use gpui::{AnyElement, Context, Div, FontWeight, div, prelude::*, px, rgb};
use gpui_component::{
    Disableable, Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
};
use remoters_core::account::DeviceRecord;

use crate::{
    Desktop, Role,
    ui::{
        ACCENT, ACCENT_LIGHT, BORDER, CANVAS, INK, MUTED, ONLINE, SIDEBAR, SIDEBAR_ACTIVE,
        SIDEBAR_HOVER, SUBTLE, platform_letter,
    },
};

use super::{
    helpers::{device_status_kind, device_status_label, format_user_subtitle, format_user_title},
    state::{AccountSelection, AuthMode, DeviceStatusKind},
};

impl Desktop {
    pub fn account_workspace_header(&self) -> (bool, String, String, bool) {
        let s = crate::i18n::tr();
        match &self.account.selection {
            AccountSelection::Auth => (
                self.account.user.is_some(),
                if self.account.auth_mode == AuthMode::Register {
                    s.new_account.to_owned()
                } else {
                    s.account.to_owned()
                },
                if self.account.auth_mode == AuthMode::Register {
                    s.title_create_account.to_owned()
                } else {
                    s.title_sign_in_remoters.to_owned()
                },
                false,
            ),
            AccountSelection::Device(_) => {
                if let Some(device) = self.account.selected_device() {
                    (
                        device.online,
                        device_status_label(device).to_owned(),
                        device.name.clone(),
                        true,
                    )
                } else {
                    (
                        false,
                        s.status_offline.to_owned(),
                        s.device_fallback.to_owned(),
                        true,
                    )
                }
            }
            AccountSelection::None => unreachable!(),
        }
    }

    pub fn account_sidebar_section(&self, cx: &mut Context<Self>) -> Div {
        let s = crate::i18n::tr();
        let signed_in = self.account.user.is_some();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child(Icon::new(IconName::CircleUser).size(px(14.)))
                            .child(s.my_devices),
                    )
                    .when(signed_in, |header| {
                        header.child(
                            Button::new("my-devices-refresh")
                                .ghost()
                                .xsmall()
                                .icon(crate::ui::refresh_icon())
                                .tooltip(s.refresh_my_devices)
                                .on_click(cx.listener(|desktop, _, _, _| {
                                    desktop.account.runtime.refresh_devices();
                                })),
                        )
                    }),
            )
            .child(if !signed_in {
                let selected = matches!(self.account.selection, AccountSelection::Auth);
                div()
                    .id("sidebar-sign-in-sync")
                    .w_full()
                    .cursor_pointer()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(if selected { ACCENT } else { BORDER }))
                    .px_2()
                    .py(px(7.))
                    .bg(rgb(if selected { ACCENT_LIGHT } else { CANVAS }))
                    .hover(move |style| {
                        style.bg(rgb(if selected {
                            ACCENT_LIGHT
                        } else {
                            SIDEBAR_HOVER
                        }))
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .on_click(cx.listener(|desktop, _, window, cx| {
                        if desktop.idle() {
                            desktop.account.open_auth(window, cx);
                            cx.notify();
                        }
                    }))
                    .child(
                        Icon::new(IconName::User)
                            .size(px(14.))
                            .flex_none()
                            .text_color(rgb(ACCENT)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .truncate()
                            .child(s.sign_in_to_sync),
                    )
                    .into_any_element()
            } else if self.account.devices.is_empty() {
                div()
                    .px_2()
                    .py(px(6.))
                    .text_xs()
                    .text_color(rgb(SUBTLE))
                    .child(s.syncing_devices)
                    .into_any_element()
            } else {
                let mut rows = Vec::with_capacity(self.account.devices.len());
                for (index, device) in self.account.devices.iter().enumerate() {
                    rows.push(self.account_device_row(index, device, cx));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows)
                    .into_any_element()
            })
    }

    fn account_device_row(
        &self,
        index: usize,
        device: &DeviceRecord,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = matches!(
            &self.account.selection,
            AccountSelection::Device(id) if id == &device.device_id
        );
        let status_kind = device_status_kind(device);
        let status_label = device_status_label(device);
        let dot_color = match status_kind {
            DeviceStatusKind::Sharing | DeviceStatusKind::Online => ONLINE,
            DeviceStatusKind::Offline => SUBTLE,
        };
        let platform_tag = platform_letter(&device.platform);
        let badge_bg = if device.online { ACCENT } else { SUBTLE };
        let device_id = device.device_id.clone();

        div()
            .id(("my-device-row", index))
            .w_full()
            .cursor_pointer()
            .rounded(px(8.))
            .px_2()
            .py(px(6.))
            .bg(rgb(if selected { SIDEBAR_ACTIVE } else { SIDEBAR }))
            .hover(move |style| {
                style.bg(rgb(if selected {
                    SIDEBAR_ACTIVE
                } else {
                    SIDEBAR_HOVER
                }))
            })
            .flex()
            .items_center()
            .gap_2()
            .on_click(cx.listener(move |desktop, _, _, cx| {
                if desktop.idle() {
                    desktop.role = Role::Connect;
                    desktop.account.selection = AccountSelection::Device(device_id.clone());
                    desktop.status = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(dot_color)),
            )
            .child(
                div()
                    .size(px(19.))
                    .flex_none()
                    .rounded(px(5.))
                    .bg(rgb(badge_bg))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(CANVAS))
                    .child(platform_tag),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::MEDIUM
                            })
                            .text_color(rgb(if device.online { INK } else { MUTED }))
                            .truncate()
                            .child(device.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(match status_kind {
                                DeviceStatusKind::Sharing => ACCENT,
                                DeviceStatusKind::Online => MUTED,
                                DeviceStatusKind::Offline => SUBTLE,
                            }))
                            .truncate()
                            .child(format!("{} · {}", device.platform, status_label)),
                    ),
            )
            .into_any_element()
    }

    pub fn account_sidebar_footer(&self, cx: &mut Context<Self>) -> Div {
        let s = crate::i18n::tr();
        if let Some(user) = &self.account.user {
            let title = format_user_title(user);
            let subtitle = format_user_subtitle(user);
            div()
                .mt_auto()
                .pt_3()
                .border_t_1()
                .border_color(rgb(BORDER))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(26.))
                                .flex_none()
                                .rounded_full()
                                .bg(rgb(ACCENT_LIGHT))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::User)
                                        .size(px(13.))
                                        .text_color(rgb(ACCENT)),
                                ),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(INK))
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(MUTED))
                                        .truncate()
                                        .child(subtitle),
                                ),
                        ),
                )
                .child(
                    Button::new("sidebar-sign-out")
                        .ghost()
                        .xsmall()
                        .label(s.sign_out)
                        .disabled(!self.idle())
                        .on_click(cx.listener(|desktop, _, _, cx| {
                            desktop.account.runtime.sign_out();
                            cx.notify();
                        })),
                )
        } else {
            let selected = matches!(self.account.selection, AccountSelection::Auth);
            div()
                .mt_auto()
                .pt_3()
                .border_t_1()
                .border_color(rgb(BORDER))
                .child(
                    div()
                        .id("sidebar-account-footer")
                        .w_full()
                        .cursor_pointer()
                        .rounded(px(8.))
                        .px_2()
                        .py(px(6.))
                        .bg(rgb(if selected { SIDEBAR_ACTIVE } else { SIDEBAR }))
                        .hover(move |style| {
                            style.bg(rgb(if selected {
                                SIDEBAR_ACTIVE
                            } else {
                                SIDEBAR_HOVER
                            }))
                        })
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .on_click(cx.listener(|desktop, _, window, cx| {
                            if desktop.idle() {
                                desktop.account.open_auth(window, cx);
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::CircleUser)
                                        .size(px(15.))
                                        .flex_none()
                                        .text_color(rgb(if selected { ACCENT } else { MUTED })),
                                )
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(if selected { INK } else { MUTED }))
                                        .child(s.account),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(ACCENT))
                                .child(s.sign_in),
                        ),
                )
        }
    }
}
