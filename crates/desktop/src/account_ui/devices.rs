use gpui::{Context, Div, FontWeight, div, prelude::*, px, rgb};
use gpui_component::{
    Disableable, IconName,
    button::{Button, ButtonVariants},
};
use remoters_core::account::DeviceRecord;

use crate::{
    Desktop, i18n,
    ui::{
        ACCENT, BORDER, CANVAS, INK, MUTED, SUBTLE, SURFACE, card, muted, platform_letter,
        status_capsule, subtle_chip,
    },
};

use super::{
    helpers::{connect_disabled_reason, device_can_connect, device_status_label},
    state::AccountSelection,
};

impl Desktop {
    pub fn render_account_workspace(&self, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        match &self.account.selection {
            AccountSelection::Auth => self.account_auth_panel(cx),
            AccountSelection::Device(_) => {
                if let Some(device) = self.account.selected_device() {
                    self.account_device_panel(device, cx)
                } else {
                    div().w_full().flex().flex_col().gap_4().child(
                        card()
                            .w_full()
                            .max_w(px(760.))
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(INK))
                                    .child(s.device_no_longer_listed),
                            )
                            .child(muted(s.device_no_longer_listed_hint)),
                    )
                }
            }
            AccountSelection::None => unreachable!(),
        }
    }

    fn account_device_panel(&self, device: &DeviceRecord, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let is_this_device = device.device_id == self.account.local_device_id();
        let status_label = device_status_label(device);
        let can_connect = device_can_connect(device);
        let disabled_reason = connect_disabled_reason(device);
        let address_display = device
            .host_address
            .as_deref()
            .filter(|addr| !addr.trim().is_empty())
            .unwrap_or(s.no_address_reported)
            .to_owned();
        let display_info = if device.display_label.trim().is_empty() {
            if device.sharing {
                s.active_sharing_session.to_owned()
            } else {
                s.not_currently_sharing.to_owned()
            }
        } else {
            device.display_label.clone()
        };

        let device_for_connect = device.clone();
        let device_id_for_remove = device.device_id.clone();

        let card_el = card()
            .w_full()
            .max_w(px(760.))
            .p_0()
            .overflow_hidden()
            .child(
                div()
                    .px_5()
                    .py_4()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(34.))
                                    .flex_none()
                                    .rounded(px(9.))
                                    .bg(rgb(if device.online { ACCENT } else { SUBTLE }))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(CANVAS))
                                    .child(platform_letter(&device.platform)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .text_size(px(15.))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(INK))
                                                    .truncate()
                                                    .child(device.name.clone()),
                                            )
                                            .when(is_this_device, |row| {
                                                row.child(subtle_chip(s.this_device))
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(device.platform.clone()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .when(device.sharing && device.view_only, |row| {
                                row.child(subtle_chip(s.view_only))
                            })
                            .child(status_capsule(status_label, device.online)),
                    ),
            )
            .child(
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(device_detail_row(s.address, address_display, true))
                            .child(device_detail_row(s.display, display_info, false)),
                    )
                    .when_some(disabled_reason, |body, reason| {
                        body.child(
                            div()
                                .rounded(px(9.))
                                .px_3()
                                .py_2()
                                .bg(rgb(SURFACE))
                                .border_1()
                                .border_color(rgb(BORDER))
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(reason),
                        )
                    })
                    .children(self.notice())
                    .child(
                        div()
                            .pt_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                Button::new("account-device-remove")
                                    .danger()
                                    .icon(IconName::Delete)
                                    .label(s.remove)
                                    .on_click(cx.listener(move |desktop, _, _, cx| {
                                        desktop
                                            .account
                                            .runtime
                                            .delete_device(device_id_for_remove.clone());
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("account-device-connect")
                                    .primary()
                                    .px_5()
                                    .disabled(!can_connect)
                                    .icon(IconName::ArrowRight)
                                    .label(s.connect_btn)
                                    .on_click(cx.listener(move |desktop, _, window, cx| {
                                        desktop.connect_to_account_device(
                                            &device_for_connect,
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    ),
            );

        div().w_full().flex().flex_col().gap_4().child(card_el)
    }
}

fn device_detail_row(label: &str, value: String, monospace: bool) -> Div {
    div()
        .p_3()
        .rounded(px(10.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(label.to_owned()),
        )
        .child(
            div()
                .min_w_0()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .when(monospace, |el| el.font_family("monospace"))
                .text_color(rgb(INK))
                .truncate()
                .child(value),
        )
}
