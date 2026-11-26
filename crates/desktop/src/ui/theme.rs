use gpui::{App, px, rgb};
use gpui_component::{Icon, Theme, ThemeMode};
use remoters_core::session::CodecPreference;

use crate::{i18n, viewer::FramePacingMode};

pub(crate) const CANVAS: u32 = 0xFFFFFF;
pub(crate) const SIDEBAR: u32 = 0xF5F5F7;
pub(crate) const SURFACE: u32 = 0xF7F7F9;
pub(crate) const SIDEBAR_ACTIVE: u32 = 0xE5E5EA;
pub(crate) const SIDEBAR_HOVER: u32 = 0xECECF0;
pub(crate) const INK: u32 = 0x1D1D1F;
pub(crate) const MUTED: u32 = 0x6E6E73;
pub(crate) const SUBTLE: u32 = 0x8E8E93;
pub(crate) const BORDER: u32 = 0xE5E5EA;
pub(crate) const ACCENT: u32 = 0x007AFF;
pub(crate) const ACCENT_LIGHT: u32 = 0xEBF3FF;
pub(crate) const ONLINE: u32 = 0x34C759;
pub(crate) const DANGER: u32 = 0xFF3B30;

pub(super) const SESSION_BACKGROUND: u32 = 0x0E0F11;
pub(super) const TITLEBAR_H: f32 = 40.0;
pub(super) const PILL_W: f32 = 114.0;
pub(super) const PILL_H: f32 = 30.0;
pub(super) const CONTROL_MARGIN: f32 = 10.0;

pub const REFRESH_ICON_PATH: &str = "icons/refresh-cw.svg";
pub const REFRESH_CW_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/></svg>"#;

pub fn refresh_icon() -> Icon {
    Icon::empty().path(REFRESH_ICON_PATH)
}

pub(super) fn codec_options() -> [(&'static str, CodecPreference); 4] {
    [
        (i18n::tr().opt_auto, CodecPreference::Auto),
        ("AV1", CodecPreference::Av1),
        ("HEVC", CodecPreference::Hevc),
        ("H.264", CodecPreference::H264),
    ]
}

pub(super) fn dynamic_range_options() -> [(&'static str, bool); 2] {
    let s = i18n::tr();
    [(s.opt_sdr, false), (s.opt_hdr_10bit, true)]
}

pub(super) fn fps_options() -> [(&'static str, Option<u16>); 4] {
    [
        (i18n::tr().opt_auto, None),
        ("60", Some(60)),
        ("90", Some(90)),
        ("120 Hz", Some(120)),
    ]
}

pub(super) fn bitrate_options() -> [(&'static str, Option<u16>); 5] {
    [
        (i18n::tr().opt_auto, None),
        ("25", Some(25)),
        ("50", Some(50)),
        ("80", Some(80)),
        ("100 Mbps", Some(100)),
    ]
}

pub(super) fn frame_pacing_options() -> [(&'static str, FramePacingMode); 2] {
    let s = i18n::tr();
    [
        (s.low_latency, FramePacingMode::LowestLatency),
        (s.balanced, FramePacingMode::Balanced),
    ]
}

pub fn configure_theme(cx: &mut App) {
    Theme::change(ThemeMode::Light, None, cx);
    let theme = Theme::global_mut(cx);
    theme.radius = px(8.);
    theme.colors.background = rgb(CANVAS).into();
    theme.colors.foreground = rgb(INK).into();
    theme.colors.muted_foreground = rgb(MUTED).into();
    theme.colors.border = rgb(BORDER).into();
    theme.colors.input = rgb(BORDER).into();
    theme.colors.ring = rgb(ACCENT).into();
    theme.colors.primary = rgb(ACCENT).into();
    theme.colors.primary_hover = rgb(0x0066D6).into();
    theme.colors.primary_active = rgb(0x0055B8).into();
    theme.colors.primary_foreground = rgb(CANVAS).into();
    theme.colors.selection = rgb(ACCENT_LIGHT).into();
    theme.colors.accent = rgb(ACCENT_LIGHT).into();
    theme.colors.title_bar = rgb(CANVAS).into();
    theme.colors.title_bar_border = rgb(BORDER).into();
}
