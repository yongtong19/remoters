use gpui::{Context, Div, FontWeight, div, prelude::*, px, rgb};
use gpui_component::{
    Icon, IconName, Sizable,
    button::{Button, ButtonVariants},
    input::Input,
    select::Select,
};

use crate::{Desktop, i18n};

use super::{
    theme::{
        ACCENT, BORDER, INK, MUTED, SURFACE, bitrate_options, codec_options, dynamic_range_options,
        fps_options, frame_pacing_options,
    },
    widgets::{card, field, muted, quality_control_row, segmented_control, subtle_chip},
};

impl Desktop {
    pub(super) fn connect_panel(&self, _narrow: bool, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let selected_address = self.address.read(cx).value().trim().to_string();
        let selected_host = self
            .discovered
            .iter()
            .find(|host| host.address.to_string() == selected_address);

        let form_card = card()
            .w_full()
            .max_w(px(760.))
            .p_0()
            .overflow_hidden()
            .when_some(selected_host, |card_el, host| {
                card_el.child(
                    div()
                        .px_5()
                        .py_3()
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
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .text_size(px(14.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(INK))
                                        .truncate()
                                        .child(host.host_name.clone()),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(MUTED)).truncate().child(
                                        format!("{} · {}", host.platform, host.display_label),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .when(host.view_only, |row| row.child(subtle_chip(s.view_only)))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family("monospace")
                                        .text_color(rgb(ACCENT))
                                        .child(host.address.to_string()),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(field(
                        s.host_address,
                        Input::new(&self.address).large().w_full(),
                    ))
                    .child(field(
                        s.pairing_code,
                        Input::new(&self.code).large().w_full(),
                    ))
                    .child(field(
                        s.display_resolution,
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(Select::new(&self.viewer_resolution).large().w_full())
                            .child(muted(s.display_resolution_hint)),
                    ))
                    .child(field(s.stream_quality, self.connect_quality_block(cx)))
                    .children(self.notice())
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("connect")
                                    .primary()
                                    .px_5()
                                    .icon(IconName::ArrowRight)
                                    .label(s.connect_btn)
                                    .on_click(cx.listener(|desktop, _, window, cx| {
                                        desktop.connect(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(feature_item(IconName::Copy, s.feature_clipboard))
                    .child(feature_item(IconName::FolderOpen, s.feature_drag_downloads))
                    .child(feature_item(IconName::Maximize, s.feature_adaptive_display))
                    .child(feature_item(IconName::Globe, s.feature_lan_vpn)),
            );

        div().w_full().flex().flex_col().gap_5().child(form_card)
    }

    fn connect_quality_block(&self, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let codecs = codec_options();
        let dynamic_ranges = dynamic_range_options();
        let fps = fps_options();
        let bitrates = bitrate_options();
        let pacings = frame_pacing_options();
        div()
            .w_full()
            .rounded(px(10.))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(BORDER))
            .px_3()
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(quality_control_row(
                s.codec,
                segmented_control(
                    "connect-codec",
                    &codecs,
                    self.codec_preference,
                    false,
                    |desktop, next, _, cx| {
                        desktop.codec_preference = next;
                        cx.notify();
                    },
                    cx,
                ),
            ))
            .child(quality_control_row(
                s.dynamic_range,
                segmented_control(
                    "connect-hdr",
                    &dynamic_ranges,
                    self.hdr_enabled,
                    false,
                    |desktop, next, _, cx| {
                        desktop.hdr_enabled = next;
                        cx.notify();
                    },
                    cx,
                ),
            ))
            .child(quality_control_row(
                s.frame_rate,
                segmented_control(
                    "connect-fps",
                    &fps,
                    self.fps_cap,
                    false,
                    |desktop, next, _, cx| {
                        desktop.fps_cap = next;
                        cx.notify();
                    },
                    cx,
                ),
            ))
            .child(quality_control_row(
                s.bitrate,
                segmented_control(
                    "connect-bitrate",
                    &bitrates,
                    self.bitrate_mbps,
                    false,
                    |desktop, next, _, cx| {
                        desktop.bitrate_mbps = next;
                        cx.notify();
                    },
                    cx,
                ),
            ))
            .child(quality_control_row(
                s.frame_pacing,
                segmented_control(
                    "connect-pacing",
                    &pacings,
                    self.frame_pacing,
                    false,
                    |desktop, next, _, cx| {
                        desktop.frame_pacing = next;
                        cx.notify();
                    },
                    cx,
                ),
            ))
    }
}

fn feature_item(icon: IconName, label: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(MUTED))
        .child(Icon::new(icon).size(px(13.)).text_color(rgb(MUTED)))
        .child(label.to_string())
}
