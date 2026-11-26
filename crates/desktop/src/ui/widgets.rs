use gpui::{Context, Div, FontWeight, Window, div, prelude::*, px, rgb, rgba};

use crate::{Desktop, metrics::SessionTelemetry};

use super::theme::{
    ACCENT, ACCENT_LIGHT, BORDER, CANVAS, DANGER, INK, MUTED, ONLINE, SIDEBAR, SUBTLE, SURFACE,
};

impl Desktop {
    pub(crate) fn notice(&self) -> Option<Div> {
        self.status.as_ref().map(|(message, error)| {
            div()
                .w_full()
                .rounded(px(9.))
                .px_3()
                .py_2()
                .text_size(px(13.))
                .bg(rgb(if *error { 0xFFECEA } else { ACCENT_LIGHT }))
                .text_color(rgb(if *error { DANGER } else { ACCENT }))
                .child(message.clone())
        })
    }
}

pub(crate) fn platform_letter(platform: &str) -> &'static str {
    let lower = platform.to_ascii_lowercase();
    if lower.contains("win") {
        "W"
    } else if lower.contains("mac") {
        "M"
    } else {
        "L"
    }
}

pub(super) fn quality_control_row(label: &str, control: Div) -> Div {
    div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(INK))
                .child(label.to_string()),
        )
        .child(control)
}

pub(super) fn segmented_control<T: Copy + Eq + 'static>(
    id_prefix: &'static str,
    options: &[(&'static str, T)],
    selected: T,
    compact: bool,
    on_select: impl Fn(&mut Desktop, T, &mut Window, &mut Context<Desktop>) + Clone + 'static,
    cx: &mut Context<Desktop>,
) -> Div {
    let mut track = div()
        .p(px(2.))
        .rounded(px(8.))
        .bg(rgb(SIDEBAR))
        .border_1()
        .border_color(rgb(BORDER))
        .flex()
        .items_center()
        .gap(px(2.));
    if compact {
        track = track.w_full();
    }
    let last_index = options.len().saturating_sub(1);
    for (index, (label, value)) in options.iter().enumerate() {
        let active = *value == selected;
        let value = *value;
        let callback = on_select.clone();
        let wide_last = compact && options.len() > 2 && index == last_index && label.len() > 4;
        let mut pill = div()
            .id((id_prefix, index))
            .cursor_pointer()
            .rounded(px(6.))
            .py(px(if compact { 4. } else { 5. }))
            .px(px(if compact {
                if wide_last { 8. } else { 4. }
            } else {
                10.
            }))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(if compact { 11. } else { 12. }))
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
                callback(desktop, value, window, cx);
            }))
            .child(*label);
        if compact && !wide_last {
            pill = pill.flex_1();
        } else if compact {
            pill = pill.flex_none();
        }
        track = track.child(pill);
    }
    track
}

pub(crate) fn format_bitrate_choice(mbps: Option<u16>) -> String {
    match mbps {
        None => crate::i18n::tr().opt_auto.to_string(),
        Some(value) => format!("{value} Mbps"),
    }
}

fn normalize_codec_label(codec: &str) -> &str {
    let trimmed = codec.trim();
    if trimmed.is_empty() || trimmed == "—" {
        crate::i18n::tr().opt_auto
    } else {
        trimmed
    }
}

pub(crate) fn quality_summary_label(codec: &str, hdr: bool, bitrate_mbps: Option<u16>) -> String {
    let codec_label = normalize_codec_label(codec);
    if hdr {
        format!(
            "{codec_label} · HDR · {}",
            format_bitrate_choice(bitrate_mbps)
        )
    } else {
        format!("{codec_label} · {}", format_bitrate_choice(bitrate_mbps))
    }
}

pub(crate) fn quality_mode_label(stats: &SessionTelemetry) -> String {
    let codec_label = normalize_codec_label(stats.codec);
    let hdr_part = if stats.hdr { " · HDR" } else { "" };
    if stats.width > 0 && stats.height > 0 && stats.target_fps > 0 {
        format!(
            "{codec_label}{hdr_part} · {}×{} · {} Hz",
            stats.width, stats.height, stats.target_fps
        )
    } else if stats.hdr {
        format!("{codec_label} · HDR")
    } else {
        codec_label.to_string()
    }
}

pub(crate) fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(14.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(CANVAS))
        .shadow_xs()
        .p_5()
}

pub(crate) fn muted(message: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .child(message.into())
}

pub(crate) fn field(label: &str, element: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .min_w_0()
        .w_full()
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgb(INK))
                .child(label.to_string()),
        )
        .child(element)
}

pub(crate) fn status_capsule(label: &str, online: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded_full()
        .py(px(3.))
        .px(px(9.))
        .bg(rgb(if online { INK } else { SIDEBAR }))
        .border_1()
        .border_color(rgb(if online { INK } else { BORDER }))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(if online { CANVAS } else { MUTED }))
        .child(
            div()
                .size(px(6.))
                .rounded_full()
                .bg(rgb(if online { ONLINE } else { SUBTLE })),
        )
        .child(label.to_string())
}

pub(crate) fn subtle_chip(label: &str) -> Div {
    div()
        .rounded_full()
        .px(px(9.))
        .py(px(3.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(BORDER))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(rgb(MUTED))
        .child(label.to_string())
}

pub(super) fn monitor_mark(width: f32, height: f32, tint: u32) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w(px(width))
                .h(px(height))
                .rounded(px(width * 0.1))
                .border_1()
                .border_color(rgb(tint)),
        )
        .child(div().w(px(width * 0.08)).h(px(height * 0.19)).bg(rgb(tint)))
        .child(div().w(px(width * 0.37)).h(px(1.)).bg(rgb(tint)))
}

#[cfg(test)]
mod tests {
    use crate::{
        i18n::{Locale, with_locale_for_test},
        metrics::SessionTelemetry,
    };

    use super::{format_bitrate_choice, quality_mode_label, quality_summary_label};

    #[test]
    fn quality_summary_and_mode_labels_format_codec_and_bitrate() {
        with_locale_for_test(Locale::En, || {
            assert_eq!(format_bitrate_choice(None), "Auto");
            assert_eq!(format_bitrate_choice(Some(80)), "80 Mbps");

            assert_eq!(quality_summary_label("HEVC", false, None), "HEVC · Auto");
            assert_eq!(
                quality_summary_label("HEVC", false, Some(80)),
                "HEVC · 80 Mbps"
            );
            assert_eq!(
                quality_summary_label("H.264", false, Some(25)),
                "H.264 · 25 Mbps"
            );
            assert_eq!(quality_summary_label("—", false, None), "Auto · Auto");
            assert_eq!(quality_summary_label("", false, Some(50)), "Auto · 50 Mbps");

            let telemetry = SessionTelemetry {
                width: 2560,
                height: 1440,
                target_fps: 120,
                codec: "HEVC",
                ..SessionTelemetry::default()
            };
            assert_eq!(quality_mode_label(&telemetry), "HEVC · 2560×1440 · 120 Hz");

            let hdr_telemetry = SessionTelemetry {
                width: 3024,
                height: 1900,
                target_fps: 120,
                codec: "AV1",
                hdr: true,
                ..SessionTelemetry::default()
            };
            assert_eq!(
                quality_mode_label(&hdr_telemetry),
                "AV1 · HDR · 3024×1900 · 120 Hz"
            );
            assert_eq!(
                quality_summary_label("AV1", true, Some(80)),
                "AV1 · HDR · 80 Mbps"
            );

            let empty_telemetry = SessionTelemetry {
                codec: "—",
                ..SessionTelemetry::default()
            };
            assert_eq!(quality_mode_label(&empty_telemetry), "Auto");
        });

        with_locale_for_test(Locale::ZhCn, || {
            assert_eq!(format_bitrate_choice(None), "自动");
            assert_eq!(quality_summary_label("HEVC", false, None), "HEVC · 自动");
            assert_eq!(quality_summary_label("—", false, None), "自动 · 自动");
            let empty_telemetry = SessionTelemetry {
                codec: "—",
                ..SessionTelemetry::default()
            };
            assert_eq!(quality_mode_label(&empty_telemetry), "自动");
        });
    }
}
