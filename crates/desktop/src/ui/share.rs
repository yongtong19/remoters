use gpui::{ClipboardItem, Context, Div, FontWeight, div, prelude::*, px, rgb};
use gpui_component::{
    IconName, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::Input,
    select::Select,
};

use crate::{Desktop, i18n, platform::display::DisplaySource, sharing::HostControlMode};

use super::{
    theme::{BORDER, INK, SURFACE},
    widgets::{card, field, muted, segmented_control, status_capsule, subtle_chip},
};

impl Desktop {
    pub(super) fn share_panel(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let virtual_selected =
            self.display.read(cx).selected_value().copied() == Some(DisplaySource::Virtual);
        let form = card()
            .w_full()
            .max_w(px(760.))
            .gap_4()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .when(narrow, |fields| fields.flex_col())
                    .child(field(
                        s.local_or_vpn_ip,
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Select::new(&self.local_ip).large().flex_1().min_w_0())
                            .child(Button::new("refresh").label(s.refresh).large().on_click(
                                cx.listener(|desktop, _, window, cx| {
                                    desktop.refresh_addresses(window, cx)
                                }),
                            )),
                    ))
                    .child(
                        div()
                            .w(px(92.))
                            .flex_none()
                            .child(field(s.port, Input::new(&self.port).large().w_full())),
                    ),
            )
            .child(field(
                s.display,
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Select::new(&self.display).large().w_full()),
                    )
                    .child(
                        Button::new("refresh-displays")
                            .flex_none()
                            .label(s.refresh)
                            .large()
                            .on_click(cx.listener(|desktop, _, window, cx| {
                                desktop.refresh_displays(window, cx)
                            })),
                    ),
            ))
            .when(virtual_selected, |form| {
                let hint = if cfg!(target_os = "windows") {
                    s.virtual_resolution_hint_win
                } else {
                    s.virtual_resolution_hint
                };
                form.child(field(
                    s.virtual_resolution,
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(Select::new(&self.virtual_resolution).large().w_full())
                        .child(muted(hint)),
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new("view-only")
                            .checked(self.view_only)
                            .label(s.view_only)
                            .on_click(cx.listener(|desktop, value, _, cx| {
                                desktop.view_only = *value;
                                cx.notify();
                            })),
                    )
                    .child(muted(s.view_only_hint).ml(px(23.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new("privacy-screen")
                            .checked(self.privacy_screen)
                            .label(s.privacy_screen)
                            .on_click(cx.listener(|desktop, value, _, cx| {
                                desktop.set_privacy_screen(*value, cx);
                            })),
                    )
                    .child(muted(s.privacy_screen_hint).ml(px(23.))),
            )
            .when(!self.view_only, |form| {
                let modes = [
                    (s.control_collaborative, HostControlMode::Collaborative),
                    (s.control_single, HostControlMode::SingleController),
                ];
                form.child(field(
                    s.multi_control_mode,
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(segmented_control(
                            "host-control-mode",
                            &modes,
                            self.host_control_mode,
                            false,
                            |desktop, next, _, cx| {
                                desktop.host_control_mode = next;
                                cx.notify();
                            },
                            cx,
                        ))
                        .child(muted(s.control_mode_hint)),
                ))
            })
            .children(self.notice())
            .child(
                div()
                    .pt_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("start-sharing")
                            .primary()
                            .px_5()
                            .label(s.start_sharing)
                            .on_click(cx.listener(|desktop, _, _, cx| desktop.share(cx))),
                    ),
            );
        div().w_full().flex().flex_col().gap_4().child(form)
    }

    pub(super) fn invitation(&self, _narrow: bool, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let sharing = self.sharing.as_ref().expect("active sharing panel");
        let address = sharing.host.address.to_string();
        let code = if self.reveal {
            sharing
                .host
                .code
                .as_bytes()
                .chunks(28)
                .map(String::from_utf8_lossy)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "•••• •••• •••• ••••".to_string()
        };
        let peer_count = sharing.peers.len();
        let connected = peer_count > 0;
        let connected_ips: Vec<String> = sharing
            .peers
            .iter()
            .map(|peer| {
                let ip = peer.ip().to_string();
                if sharing.controller == Some(*peer)
                    && self.host_control_mode == HostControlMode::SingleController
                {
                    format!("{ip} ★")
                } else {
                    ip
                }
            })
            .collect();
        let form = card()
            .w_full()
            .max_w(px(760.))
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(s.invitation),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .children(self.privacy_screen_status_label().map(subtle_chip))
                            .when(sharing._beacon.is_some(), |row| {
                                row.child(subtle_chip(s.discoverable_on_lan))
                            })
                            .child(status_capsule(
                                &if connected {
                                    i18n::fmt_connected_devices(peer_count)
                                } else {
                                    s.waiting_for_guest.to_string()
                                },
                                connected,
                            )),
                    ),
            )
            .when(!connected_ips.is_empty(), |card| {
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .flex_wrap()
                        .children(connected_ips.iter().map(|ip| subtle_chip(ip))),
                )
            })
            .child(field(
                s.display,
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .flex_wrap()
                    .p_3()
                    .rounded(px(10.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(INK))
                            .child(sharing.display_summary.clone()),
                    )
                    .children(
                        sharing
                            .active_format
                            .map(|(w, h, fps)| subtle_chip(&format!("{w} × {h} @ {fps} Hz"))),
                    ),
            ))
            .child(invite_field(
                s.host_address,
                address,
                Button::new("copy-address")
                    .small()
                    .icon(IconName::Copy)
                    .label(s.copy)
                    .on_click(cx.listener(|desktop, _, _, cx| desktop.copy_invite(false, cx))),
            ))
            .child(invite_field(
                s.pairing_code,
                code,
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new("reveal-code")
                            .ghost()
                            .small()
                            .icon(if self.reveal {
                                IconName::EyeOff
                            } else {
                                IconName::Eye
                            })
                            .label(if self.reveal { s.hide } else { s.show })
                            .on_click(cx.listener(|desktop, _, _, cx| {
                                desktop.reveal = !desktop.reveal;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("copy-code")
                            .small()
                            .icon(IconName::Copy)
                            .label(s.copy)
                            .on_click(
                                cx.listener(|desktop, _, _, cx| desktop.copy_invite(true, cx)),
                            ),
                    ),
            ))
            .child(
                div()
                    .p_3()
                    .rounded(px(10.))
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new("active-privacy-screen")
                            .checked(self.privacy_screen)
                            .label(s.privacy_screen)
                            .on_click(cx.listener(|desktop, value, _, cx| {
                                desktop.set_privacy_screen(*value, cx);
                            })),
                    )
                    .child(muted(s.privacy_screen_hint).ml(px(23.))),
            )
            .children(self.notice())
            .child(
                div()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(muted(if self.view_only {
                        s.view_only_access
                    } else if self.host_control_mode == HostControlMode::SingleController {
                        s.single_controller_access
                    } else {
                        s.collaborative_control_access
                    }))
                    .child(
                        Button::new("stop-sharing")
                            .danger()
                            .label(s.stop_sharing)
                            .on_click(cx.listener(|desktop, _, _, cx| {
                                desktop.sharing = None;
                                desktop.reveal = false;
                                desktop.status = Some((i18n::tr().sharing_stopped.into(), false));
                                desktop.sync_account_sharing();
                                cx.notify();
                            })),
                    ),
            );
        div().w_full().flex().flex_col().gap_4().child(form)
    }

    fn copy_invite(&mut self, pairing: bool, cx: &mut Context<Self>) {
        if let Some(sharing) = &self.sharing {
            let s = i18n::tr();
            let (value, message) = if pairing {
                (sharing.host.code.clone(), s.pairing_code_copied)
            } else {
                (sharing.host.address.to_string(), s.address_copied)
            };
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.status = Some((message.into(), false));
            cx.notify();
        }
    }
}

fn invite_field(label: &str, value: String, controls: impl IntoElement) -> Div {
    field(
        label,
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(10.))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_size(px(13.))
                    .font_family("monospace")
                    .overflow_hidden()
                    .child(value),
            )
            .child(div().flex().justify_end().child(controls)),
    )
}
