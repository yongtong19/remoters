use gpui::{Context, CursorStyle, Div, FontWeight, div, prelude::*, px, rgb, rgba};

use crate::{
    Desktop, Viewer, i18n,
    metrics::{SessionTelemetry, format_bitrate, format_byte_rate, format_latency},
    platform::display::{ViewerResolution, VirtualSpec},
};

use super::{
    theme::{
        ACCENT, BORDER, CANVAS, INK, MUTED, SIDEBAR, SUBTLE, SURFACE, bitrate_options, fps_options,
        frame_pacing_options,
    },
    widgets::{format_bitrate_choice, quality_mode_label, segmented_control},
};

pub(super) fn stats_inset(stats: &SessionTelemetry) -> Div {
    let s = i18n::tr();
    div()
        .mx(px(6.))
        .my(px(3.))
        .px(px(10.))
        .py(px(8.))
        .rounded(px(10.))
        .bg(rgba(0x00000008))
        .border_1()
        .border_color(rgba(0x0000000D))
        .flex()
        .flex_col()
        .gap(px(5.))
        .child(stats_row(s.codec, quality_mode_label(stats)))
        .child(stats_row(s.frame_rate, format!("{:.1} fps", stats.fps)))
        .child(stats_row(s.bitrate, format_bitrate(stats.video_bps)))
        .child(stats_row(
            s.network,
            format!(
                "↓ {} · ↑ {}",
                format_byte_rate(stats.net_rx_bytes_per_sec),
                format_byte_rate(stats.net_tx_bytes_per_sec)
            ),
        ))
        .child(stats_row(s.e2e_latency, format_latency(stats.e2e_ms)))
        .child(stats_row(
            s.encode_decode,
            format!(
                "{} / {}",
                format_latency(stats.encode_ms),
                format_latency(stats.decode_ms)
            ),
        ))
        .child(stats_row(
            s.rtt_loss,
            i18n::fmt_rtt_loss(&format_latency(stats.rtt_ms), stats.lost_packets),
        ))
}

pub(super) fn quality_inset(viewer: &Viewer, cx: &mut Context<Desktop>) -> Div {
    let s = i18n::tr();
    let fps_choices = fps_options();
    let bitrates = bitrate_options();
    let pacings = frame_pacing_options();
    let fixed_row_1 = [
        (
            "1920 × 1080",
            ViewerResolution::Fixed(VirtualSpec {
                width: 1920,
                height: 1080,
                fps: 60,
            }),
        ),
        (
            "2560 × 1440",
            ViewerResolution::Fixed(VirtualSpec {
                width: 2560,
                height: 1440,
                fps: 60,
            }),
        ),
    ];
    let fixed_row_2 = [
        (
            "2560 × 1600",
            ViewerResolution::Fixed(VirtualSpec {
                width: 2560,
                height: 1600,
                fps: 60,
            }),
        ),
        (
            "3840 × 2160 (4K)",
            ViewerResolution::Fixed(VirtualSpec {
                width: 3840,
                height: 2160,
                fps: 60,
            }),
        ),
    ];
    div()
        .mx(px(6.))
        .my(px(3.))
        .px(px(10.))
        .py(px(8.))
        .rounded(px(10.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .flex()
        .flex_col()
        .gap(px(7.))
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_xs()
                .child(div().flex_none().text_color(rgb(MUTED)).child(s.stream))
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .font_family("monospace")
                        .text_color(rgb(INK))
                        .truncate()
                        .child(quality_mode_label(&viewer.telemetry)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(s.resolution),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(ACCENT))
                                .truncate()
                                .child(format_resolution_choice(viewer.resolution)),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .p(px(2.))
                        .rounded(px(8.))
                        .bg(rgb(SIDEBAR))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(resolution_pill(
                            "session-res-match",
                            s.match_this_display,
                            ViewerResolution::MatchLocal,
                            viewer.resolution,
                            cx,
                        ))
                        .child(resolution_pill_row(
                            "session-res-row1",
                            &fixed_row_1,
                            viewer.resolution,
                            cx,
                        ))
                        .child(resolution_pill_row(
                            "session-res-row2",
                            &fixed_row_2,
                            viewer.resolution,
                            cx,
                        ))
                        .child(resolution_pill(
                            "session-res-host",
                            s.host_default,
                            ViewerResolution::HostDefault,
                            viewer.resolution,
                            cx,
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(div().text_xs().text_color(rgb(MUTED)).child(s.max_fps))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(ACCENT))
                                .child(format_fps_choice(viewer.fps_cap)),
                        ),
                )
                .child(segmented_control(
                    "session-quality-fps",
                    &fps_choices,
                    viewer.fps_cap,
                    true,
                    |desktop, next, window, cx| {
                        desktop.set_active_fps_cap(next, window, cx);
                    },
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(div().text_xs().text_color(rgb(MUTED)).child(s.bitrate))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(ACCENT))
                                .child(format_bitrate_choice(viewer.active_bitrate_mbps)),
                        ),
                )
                .child(segmented_control(
                    "session-quality-bitrate",
                    &bitrates,
                    viewer.active_bitrate_mbps,
                    true,
                    |desktop, next, window, cx| {
                        desktop.set_active_bitrate_mbps(next, window, cx);
                    },
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(div().text_xs().text_color(rgb(MUTED)).child(s.frame_pacing))
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(ACCENT))
                                .child(i18n::frame_pacing_label(viewer.frame_pacing)),
                        ),
                )
                .child(segmented_control(
                    "session-quality-pacing",
                    &pacings,
                    viewer.frame_pacing,
                    true,
                    |desktop, next, window, cx| {
                        desktop.set_frame_pacing(next, window, cx);
                    },
                    cx,
                )),
        )
}

fn format_resolution_choice(choice: ViewerResolution) -> String {
    let s = i18n::tr();
    match choice {
        ViewerResolution::MatchLocal => s.match_this_display.to_string(),
        ViewerResolution::Fixed(spec) => format!("{} × {}", spec.width, spec.height),
        ViewerResolution::HostDefault => s.host_default.to_string(),
    }
}

fn format_fps_choice(fps_cap: Option<u16>) -> String {
    match fps_cap {
        None => i18n::tr().opt_auto.to_string(),
        Some(fps) => format!("{fps} Hz"),
    }
}

fn resolution_pill(
    id: &'static str,
    label: &'static str,
    value: ViewerResolution,
    selected: ViewerResolution,
    cx: &mut Context<Desktop>,
) -> gpui::Stateful<Div> {
    let active = value == selected;
    div()
        .id(id)
        .w_full()
        .min_w_0()
        .cursor_pointer()
        .rounded(px(6.))
        .py(px(4.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .text_color(rgb(if active { ACCENT } else { MUTED }))
        .when(active, |el| {
            el.bg(rgb(CANVAS))
                .border_1()
                .border_color(rgb(BORDER))
                .shadow_xs()
        })
        .when(!active, |el| {
            el.border_1()
                .border_color(rgba(0x00000000))
                .hover(|style| style.text_color(rgb(INK)))
        })
        .on_click(cx.listener(move |desktop, _, window, cx| {
            desktop.set_active_resolution(value, window, cx);
        }))
        .child(div().truncate().child(label))
}

fn resolution_pill_row(
    id_prefix: &'static str,
    options: &[(&'static str, ViewerResolution); 2],
    selected: ViewerResolution,
    cx: &mut Context<Desktop>,
) -> Div {
    let mut row = div().w_full().flex().items_center().gap(px(2.));
    for (index, &(label, value)) in options.iter().enumerate() {
        let active = value == selected;
        row = row.child(
            div()
                .id((id_prefix, index))
                .flex_1()
                .min_w_0()
                .cursor_pointer()
                .rounded(px(6.))
                .py(px(4.))
                .px(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(rgb(if active { ACCENT } else { MUTED }))
                .when(active, |el| {
                    el.bg(rgb(CANVAS))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .shadow_xs()
                })
                .when(!active, |el| {
                    el.border_1()
                        .border_color(rgba(0x00000000))
                        .hover(|style| style.text_color(rgb(INK)))
                })
                .on_click(cx.listener(move |desktop, _, window, cx| {
                    desktop.set_active_resolution(value, window, cx);
                }))
                .child(div().truncate().child(label)),
        );
    }
    row
}

pub(super) fn stats_hud(stats: &SessionTelemetry) -> Div {
    let s = i18n::tr();
    div()
        .w(px(236.))
        .cursor(CursorStyle::Arrow)
        .flex()
        .flex_col()
        .gap(px(5.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(12.))
        .shadow_lg()
        .border_1()
        .border_color(rgba(0x00000018))
        .bg(rgba(0xFAFAFCDE))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(INK))
                        .child(s.live_stats),
                )
                .child(div().text_color(rgb(SUBTLE)).child(if stats.hdr {
                    format!("{} · HDR · {}×{}", stats.codec, stats.width, stats.height)
                } else {
                    format!("{} · {}×{}", stats.codec, stats.width, stats.height)
                })),
        )
        .child(stats_row(s.frame_rate, format!("{:.1} fps", stats.fps)))
        .child(stats_row(s.bitrate, format_bitrate(stats.video_bps)))
        .child(stats_row(
            s.network,
            format!(
                "↓ {} · ↑ {}",
                format_byte_rate(stats.net_rx_bytes_per_sec),
                format_byte_rate(stats.net_tx_bytes_per_sec)
            ),
        ))
        .child(stats_row(s.e2e_latency, format_latency(stats.e2e_ms)))
        .child(stats_row(
            s.encode_decode,
            format!(
                "{} / {}",
                format_latency(stats.encode_ms),
                format_latency(stats.decode_ms)
            ),
        ))
        .child(stats_row(
            s.rtt_loss,
            i18n::fmt_rtt_loss(&format_latency(stats.rtt_ms), stats.lost_packets),
        ))
}

fn stats_row(label: &str, value: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .text_xs()
        .child(div().text_color(rgb(MUTED)).child(label.to_string()))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .font_family("monospace")
                .text_color(rgb(INK))
                .child(value),
        )
}
