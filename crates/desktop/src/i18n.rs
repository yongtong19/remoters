mod en;
mod es;
mod fr;
mod ja;
mod ko;
mod zh_cn;

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
};

use crate::viewer::FramePacingMode;

const UNINITIALIZED: u8 = 0;
const LOCALE_EN: u8 = 1;
const LOCALE_ZH_CN: u8 = 2;
const LOCALE_ES: u8 = 3;
const LOCALE_FR: u8 = 4;
const LOCALE_JA: u8 = 5;
const LOCALE_KO: u8 = 6;

static ACTIVE_LOCALE: AtomicU8 = AtomicU8::new(UNINITIALIZED);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locale {
    En,
    ZhCn,
    Es,
    Fr,
    Ja,
    Ko,
}

impl Locale {
    pub const ALL: [Self; 6] = [Self::En, Self::ZhCn, Self::Es, Self::Fr, Self::Ja, Self::Ko];

    pub fn as_code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::Ja => "ja",
            Self::Ko => "ko",
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::ZhCn => "简体中文",
            Self::Es => "Español",
            Self::Fr => "Français",
            Self::Ja => "日本語",
            Self::Ko => "한국어",
        }
    }

    pub fn short_label(self) -> &'static str {
        strings_for(self).lang_short
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "c" || lower == "posix" || lower.starts_with("c.") {
            return None;
        }
        let primary = lower.split(['-', '_', '.', '@']).next().unwrap_or("");
        match primary {
            "en" => Some(Self::En),
            "zh" => Some(Self::ZhCn),
            "es" => Some(Self::Es),
            "fr" => Some(Self::Fr),
            "ja" => Some(Self::Ja),
            "ko" => Some(Self::Ko),
            _ => None,
        }
    }

    fn to_raw(self) -> u8 {
        match self {
            Self::En => LOCALE_EN,
            Self::ZhCn => LOCALE_ZH_CN,
            Self::Es => LOCALE_ES,
            Self::Fr => LOCALE_FR,
            Self::Ja => LOCALE_JA,
            Self::Ko => LOCALE_KO,
        }
    }

    fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            LOCALE_EN => Some(Self::En),
            LOCALE_ZH_CN => Some(Self::ZhCn),
            LOCALE_ES => Some(Self::Es),
            LOCALE_FR => Some(Self::Fr),
            LOCALE_JA => Some(Self::Ja),
            LOCALE_KO => Some(Self::Ko),
            _ => None,
        }
    }
}

#[allow(dead_code)]
pub struct Strings {
    // Top bar & multi-session tab bar
    pub app_name: &'static str,
    pub lan_vpn_chip: &'static str,
    pub home_tab: &'static str,
    pub new_session: &'static str,
    pub close_tab: &'static str,
    pub language: &'static str,
    pub lang_short: &'static str,
    pub lang_switch_tooltip: &'static str,

    // Sidebar & workspace header
    pub my_devices: &'static str,
    pub refresh_my_devices: &'static str,
    pub sign_in_to_sync: &'static str,
    pub syncing_devices: &'static str,
    pub nearby_devices: &'static str,
    pub scan_nearby_devices: &'static str,
    pub listening_on_lan: &'static str,
    pub remote_section: &'static str,
    pub nav_connect: &'static str,
    pub nav_share: &'static str,
    pub account: &'static str,
    pub new_account: &'static str,
    pub sign_in: &'static str,
    pub sign_out: &'static str,
    pub refresh: &'static str,
    pub scan: &'static str,
    pub refresh_network_displays: &'static str,
    pub status_connected: &'static str,
    pub status_online: &'static str,
    pub status_offline: &'static str,
    pub status_sharing: &'static str,
    pub status_ready: &'static str,
    pub status_listening: &'static str,
    pub title_share_this_device: &'static str,
    pub title_connect_to_device: &'static str,
    pub title_create_account: &'static str,
    pub title_sign_in_remoters: &'static str,
    pub device_fallback: &'static str,

    // Connect panel & stream quality options
    pub host_address: &'static str,
    pub pairing_code: &'static str,
    pub pairing_code_placeholder: &'static str,
    pub display_resolution: &'static str,
    pub display_resolution_hint: &'static str,
    pub stream_quality: &'static str,
    pub connect_btn: &'static str,
    pub view_only: &'static str,
    pub feature_clipboard: &'static str,
    pub feature_drag_downloads: &'static str,
    pub feature_adaptive_display: &'static str,
    pub feature_lan_vpn: &'static str,
    pub codec: &'static str,
    pub dynamic_range: &'static str,
    pub frame_rate: &'static str,
    pub bitrate: &'static str,
    pub frame_pacing: &'static str,
    pub opt_auto: &'static str,
    pub opt_sdr: &'static str,
    pub opt_hdr_10bit: &'static str,
    pub low_latency: &'static str,
    pub balanced: &'static str,

    // Share & invitation panels
    pub local_or_vpn_ip: &'static str,
    pub port: &'static str,
    pub display: &'static str,
    pub virtual_resolution: &'static str,
    pub virtual_resolution_hint_win: &'static str,
    pub virtual_resolution_hint: &'static str,
    pub view_only_hint: &'static str,
    pub privacy_screen: &'static str,
    pub privacy_screen_hint: &'static str,
    pub privacy_screen_active: &'static str,
    pub privacy_screen_armed: &'static str,
    pub start_sharing: &'static str,
    pub invitation: &'static str,
    pub discoverable_on_lan: &'static str,
    pub waiting_for_guest: &'static str,
    pub copy: &'static str,
    pub show: &'static str,
    pub hide: &'static str,
    pub view_only_access: &'static str,
    pub full_control_access: &'static str,
    pub multi_control_mode: &'static str,
    pub control_collaborative: &'static str,
    pub control_single: &'static str,
    pub control_mode_hint: &'static str,
    pub single_controller_access: &'static str,
    pub collaborative_control_access: &'static str,
    pub stop_sharing: &'static str,
    pub sharing_stopped: &'static str,
    pub pairing_code_copied: &'static str,
    pub address_copied: &'static str,
    pub err_select_local_ip: &'static str,
    pub err_invalid_port: &'static str,
    pub err_select_display: &'static str,
    pub sharing_failed_to_start: &'static str,

    // Session view, control pill, menu, overlays & stats
    pub waiting_for_remote_display: &'static str,
    pub session_controlling: &'static str,
    pub session_connecting: &'static str,
    pub session_connected: &'static str,
    pub session_viewing: &'static str,
    pub session_disconnected: &'static str,
    pub control_pill: &'static str,
    pub take_control: &'static str,
    pub release_input: &'static str,
    pub stats: &'static str,
    pub quality: &'static str,
    pub resolution: &'static str,
    pub max_fps: &'static str,
    pub pin_stats_overlay: &'static str,
    pub hide_stats_overlay: &'static str,
    pub gamepad: &'static str,
    pub mute_audio: &'static str,
    pub unmute_audio: &'static str,
    pub muted: &'static str,
    pub fullscreen: &'static str,
    pub exit_fullscreen: &'static str,
    pub disconnect: &'static str,
    pub cancel: &'static str,
    pub stream: &'static str,
    pub live_stats: &'static str,
    pub network: &'static str,
    pub e2e_latency: &'static str,
    pub encode_decode: &'static str,
    pub rtt_loss: &'static str,
    pub send_files: &'static str,
    pub view_only_file_drop_disabled: &'static str,

    // Account UI (auth & devices)
    pub create_account: &'static str,
    pub auth_subtitle: &'static str,
    pub server_url: &'static str,
    pub username: &'static str,
    pub username_placeholder: &'static str,
    pub display_name: &'static str,
    pub display_name_placeholder: &'static str,
    pub password: &'static str,
    pub creating_account: &'static str,
    pub signing_in: &'static str,
    pub err_enter_server_url: &'static str,
    pub err_enter_username: &'static str,
    pub err_enter_password: &'static str,
    pub device_removed: &'static str,
    pub device_no_longer_listed: &'static str,
    pub device_no_longer_listed_hint: &'static str,
    pub this_device: &'static str,
    pub address: &'static str,
    pub no_address_reported: &'static str,
    pub active_sharing_session: &'static str,
    pub not_currently_sharing: &'static str,
    pub remove: &'static str,
    pub err_device_offline: &'static str,
    pub err_device_not_sharing: &'static str,
    pub err_device_no_address: &'static str,
    pub err_device_no_pairing_code: &'static str,
    pub err_device_invalid_pairing_code: &'static str,

    // Display & resolution labels
    pub main_display: &'static str,
    pub new_virtual_display: &'static str,
    pub new_virtual_display_sudovda: &'static str,
    pub match_viewer_adaptive: &'static str,
    pub match_this_display: &'static str,
    pub host_default: &'static str,
    pub virtual_display_match_viewer: &'static str,
    pub virtual_display: &'static str,
}

pub fn locale() -> Locale {
    if let Some(current) = Locale::from_raw(ACTIVE_LOCALE.load(Ordering::Relaxed)) {
        return current;
    }
    let detected = detect_system_locale();
    match ACTIVE_LOCALE.compare_exchange(
        UNINITIALIZED,
        detected.to_raw(),
        Ordering::SeqCst,
        Ordering::Relaxed,
    ) {
        Ok(_) => detected,
        Err(existing) => Locale::from_raw(existing).unwrap_or(detected),
    }
}

pub fn set_locale(next: Locale) {
    ACTIVE_LOCALE.store(next.to_raw(), Ordering::SeqCst);
    if let Some(path) = locale_config_path() {
        let _ = save_locale_to_path(&path, next);
    }
}

pub fn strings_for(loc: Locale) -> &'static Strings {
    match loc {
        Locale::En => &en::STRINGS,
        Locale::ZhCn => &zh_cn::STRINGS,
        Locale::Es => &es::STRINGS,
        Locale::Fr => &fr::STRINGS,
        Locale::Ja => &ja::STRINGS,
        Locale::Ko => &ko::STRINGS,
    }
}

pub fn tr() -> &'static Strings {
    strings_for(locale())
}

pub fn frame_pacing_label(mode: FramePacingMode) -> &'static str {
    let s = tr();
    match mode {
        FramePacingMode::LowestLatency => s.low_latency,
        FramePacingMode::Balanced => s.balanced,
    }
}

#[allow(dead_code)]
pub fn fmt_sharing_with(peer_ip: &str) -> String {
    match locale() {
        Locale::En => format!("Sharing with {peer_ip}"),
        Locale::ZhCn => format!("正在与 {peer_ip} 共享"),
        Locale::Es => format!("Compartiendo con {peer_ip}"),
        Locale::Fr => format!("Partage avec {peer_ip}"),
        Locale::Ja => format!("{peer_ip} と共有中"),
        Locale::Ko => format!("{peer_ip}에 공유 중"),
    }
}

#[allow(dead_code)]
pub fn fmt_connected_devices(count: usize) -> String {
    match locale() {
        Locale::En => {
            if count == 1 {
                "1 connected".to_string()
            } else {
                format!("{count} connected")
            }
        }
        Locale::ZhCn => format!("已连接 {count} 台设备"),
        Locale::Es => {
            if count == 1 {
                "1 conectado".to_string()
            } else {
                format!("{count} conectados")
            }
        }
        Locale::Fr => {
            if count == 1 {
                "1 connecté".to_string()
            } else {
                format!("{count} connectés")
            }
        }
        Locale::Ja => format!("{count} 台接続中"),
        Locale::Ko => format!("{count}대 연결됨"),
    }
}

#[allow(dead_code)]
pub fn fmt_online_count(count: usize) -> String {
    match locale() {
        Locale::En => format!("{count} online"),
        Locale::ZhCn => format!("{count} 台在线"),
        Locale::Es => format!("{count} en línea"),
        Locale::Fr => format!("{count} en ligne"),
        Locale::Ja => format!("{count} 台オンライン"),
        Locale::Ko => format!("{count}대 온라인"),
    }
}

#[allow(dead_code)]
pub fn fmt_gamepads_connected(count: u8) -> String {
    match locale() {
        Locale::En => format!("{count} connected"),
        Locale::ZhCn => format!("已连接 {count} 个"),
        Locale::Es => {
            if count == 1 {
                "1 conectado".to_string()
            } else {
                format!("{count} conectados")
            }
        }
        Locale::Fr => {
            if count == 1 {
                "1 connecté".to_string()
            } else {
                format!("{count} connectés")
            }
        }
        Locale::Ja => format!("{count} 台接続中"),
        Locale::Ko => format!("{count}개 연결됨"),
    }
}

pub fn fmt_rtt_loss(rtt: &str, lost: u64) -> String {
    match locale() {
        Locale::En => format!("{rtt} · {lost} lost"),
        Locale::ZhCn => format!("{rtt} · 丢包 {lost}"),
        Locale::Es => {
            if lost == 1 {
                format!("{rtt} · 1 perdido")
            } else {
                format!("{rtt} · {lost} perdidos")
            }
        }
        Locale::Fr => {
            if lost == 1 {
                format!("{rtt} · 1 perdu")
            } else {
                format!("{rtt} · {lost} perdus")
            }
        }
        Locale::Ja => format!("{rtt} · 損失 {lost}"),
        Locale::Ko => format!("{rtt} · 손실 {lost}"),
    }
}

#[allow(dead_code)]
pub fn fmt_sending_files(count: usize) -> String {
    match locale() {
        Locale::En => format!("Sending {count} file(s)…"),
        Locale::ZhCn => format!("正在发送 {count} 个文件…"),
        Locale::Es => {
            if count == 1 {
                "Enviando 1 archivo…".to_string()
            } else {
                format!("Enviando {count} archivos…")
            }
        }
        Locale::Fr => {
            if count == 1 {
                "Envoi de 1 fichier…".to_string()
            } else {
                format!("Envoi de {count} fichiers…")
            }
        }
        Locale::Ja => format!("{count} 件のファイルを送信中…"),
        Locale::Ko => format!("파일 {count}개 전송 중…"),
    }
}

#[allow(dead_code)]
pub fn fmt_saved_file(relative_path: &str) -> String {
    match locale() {
        Locale::En => format!("Saved {relative_path} to Downloads."),
        Locale::ZhCn => format!("已保存 {relative_path} 至下载目录。"),
        Locale::Es => format!("{relative_path} guardado en Descargas."),
        Locale::Fr => format!("{relative_path} enregistré dans Téléchargements."),
        Locale::Ja => format!("{relative_path} をダウンロードに保存しました。"),
        Locale::Ko => format!("{relative_path} 파일을 다운로드 폴더에 저장했습니다."),
    }
}

#[allow(dead_code)]
pub fn fmt_session_ended(err: &str) -> String {
    match locale() {
        Locale::En => format!("Session ended. {err}"),
        Locale::ZhCn => format!("会话已结束。{err}"),
        Locale::Es => format!("Sesión finalizada. {err}"),
        Locale::Fr => format!("Session terminée. {err}"),
        Locale::Ja => format!("セッションが終了しました。{err}"),
        Locale::Ko => format!("세션이 종료되었습니다. {err}"),
    }
}

pub fn fmt_invalid_device_address(raw: &str) -> String {
    match locale() {
        Locale::En => format!("Invalid device address `{raw}`."),
        Locale::ZhCn => format!("无效的设备地址 `{raw}`。"),
        Locale::Es => format!("Dirección de dispositivo no válida: `{raw}`."),
        Locale::Fr => format!("Adresse d’appareil invalide : `{raw}`."),
        Locale::Ja => format!("無効なデバイスアドレスです: `{raw}`。"),
        Locale::Ko => format!("유효하지 않은 기기 주소입니다: `{raw}`."),
    }
}

#[allow(dead_code)]
pub fn fmt_main_display_size(width: u32, height: u32) -> String {
    format!("{} · {width} × {height}", tr().main_display)
}

#[allow(dead_code)]
pub fn fmt_virtual_display_size(width: u32, height: u32) -> String {
    format!("{} · {width} × {height}", tr().virtual_display)
}

pub fn fmt_match_this_display_size(width: u16, height: u16) -> String {
    format!("{} · {width} × {height}", tr().match_this_display)
}

pub fn detect_system_locale() -> Locale {
    if let Some(path) = locale_config_path()
        && let Some(saved) = load_locale_from_path(&path)
    {
        return saved;
    }
    detect_locale_from_sources(
        std::env::var("REMOTERS_LANG").ok().as_deref(),
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LC_MESSAGES").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
        os_system_locale().as_deref(),
    )
}

fn detect_locale_from_sources(
    remoters_lang: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
    os_fallback: Option<&str>,
) -> Locale {
    if let Some(parsed) = remoters_lang.and_then(Locale::parse) {
        return parsed;
    }
    for env_val in [lc_all, lc_messages, lang].into_iter().flatten() {
        let trimmed = env_val.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "c" || lower == "posix" || lower.starts_with("c.") {
            continue;
        }
        return Locale::parse(trimmed).unwrap_or(Locale::En);
    }
    os_fallback.and_then(Locale::parse).unwrap_or(Locale::En)
}

fn locale_config_path() -> Option<PathBuf> {
    config_base_dir().map(|dir| dir.join("remoters").join("locale"))
}

fn config_base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
            && !xdg.is_empty()
        {
            return Some(PathBuf::from(xdg));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
    }
}

fn load_locale_from_path(path: &Path) -> Option<Locale> {
    let content = std::fs::read_to_string(path).ok()?;
    Locale::parse(&content)
}

fn save_locale_to_path(path: &Path, locale: Locale) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, locale.as_code())
}

#[cfg(target_os = "macos")]
fn os_system_locale() -> Option<String> {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            let candidate = line.trim().trim_matches(&['(', ')', '"', ','][..]).trim();
            if !candidate.is_empty() {
                return Some(candidate.to_owned());
            }
        }
    }
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "windows")]
fn os_system_locale() -> Option<String> {
    unsafe extern "system" {
        fn GetUserDefaultLocaleName(lpLocaleName: *mut u16, cchLocaleName: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 1 {
        return None;
    }
    String::from_utf16(&buf[..(len as usize - 1)]).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn os_system_locale() -> Option<String> {
    None
}

#[cfg(test)]
pub(crate) fn with_locale_for_test<R>(loc: Locale, f: impl FnOnce() -> R) -> R {
    use std::sync::Mutex;
    static TEST_LOCALE_LOCK: Mutex<()> = Mutex::new(());
    let _guard = TEST_LOCALE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let prev = ACTIVE_LOCALE.swap(loc.to_raw(), Ordering::SeqCst);
    let result = f();
    ACTIVE_LOCALE.store(prev, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        Locale, Strings, detect_locale_from_sources, fmt_gamepads_connected,
        fmt_invalid_device_address, fmt_online_count, fmt_rtt_loss, fmt_saved_file,
        fmt_sending_files, fmt_session_ended, fmt_sharing_with, load_locale_from_path, locale,
        save_locale_to_path, strings_for, tr, with_locale_for_test,
    };

    fn all_string_values(s: &Strings) -> Vec<&'static str> {
        let Strings {
            app_name,
            lan_vpn_chip,
            home_tab,
            new_session,
            close_tab,
            language,
            lang_short,
            lang_switch_tooltip,
            my_devices,
            refresh_my_devices,
            sign_in_to_sync,
            syncing_devices,
            nearby_devices,
            scan_nearby_devices,
            listening_on_lan,
            remote_section,
            nav_connect,
            nav_share,
            account,
            new_account,
            sign_in,
            sign_out,
            refresh,
            scan,
            refresh_network_displays,
            status_connected,
            status_online,
            status_offline,
            status_sharing,
            status_ready,
            status_listening,
            title_share_this_device,
            title_connect_to_device,
            title_create_account,
            title_sign_in_remoters,
            device_fallback,
            host_address,
            pairing_code,
            pairing_code_placeholder,
            display_resolution,
            display_resolution_hint,
            stream_quality,
            connect_btn,
            view_only,
            feature_clipboard,
            feature_drag_downloads,
            feature_adaptive_display,
            feature_lan_vpn,
            codec,
            dynamic_range,
            frame_rate,
            bitrate,
            frame_pacing,
            opt_auto,
            opt_sdr,
            opt_hdr_10bit,
            low_latency,
            balanced,
            local_or_vpn_ip,
            port,
            display,
            virtual_resolution,
            virtual_resolution_hint_win,
            virtual_resolution_hint,
            view_only_hint,
            privacy_screen,
            privacy_screen_hint,
            privacy_screen_active,
            privacy_screen_armed,
            start_sharing,
            invitation,
            discoverable_on_lan,
            waiting_for_guest,
            copy,
            show,
            hide,
            view_only_access,
            full_control_access,
            multi_control_mode,
            control_collaborative,
            control_single,
            control_mode_hint,
            single_controller_access,
            collaborative_control_access,
            stop_sharing,
            sharing_stopped,
            pairing_code_copied,
            address_copied,
            err_select_local_ip,
            err_invalid_port,
            err_select_display,
            sharing_failed_to_start,
            waiting_for_remote_display,
            session_controlling,
            session_connecting,
            session_connected,
            session_viewing,
            session_disconnected,
            control_pill,
            take_control,
            release_input,
            stats,
            quality,
            resolution,
            max_fps,
            pin_stats_overlay,
            hide_stats_overlay,
            gamepad,
            mute_audio,
            unmute_audio,
            muted,
            fullscreen,
            exit_fullscreen,
            disconnect,
            cancel,
            stream,
            live_stats,
            network,
            e2e_latency,
            encode_decode,
            rtt_loss,
            send_files,
            view_only_file_drop_disabled,
            create_account,
            auth_subtitle,
            server_url,
            username,
            username_placeholder,
            display_name,
            display_name_placeholder,
            password,
            creating_account,
            signing_in,
            err_enter_server_url,
            err_enter_username,
            err_enter_password,
            device_removed,
            device_no_longer_listed,
            device_no_longer_listed_hint,
            this_device,
            address,
            no_address_reported,
            active_sharing_session,
            not_currently_sharing,
            remove,
            err_device_offline,
            err_device_not_sharing,
            err_device_no_address,
            err_device_no_pairing_code,
            err_device_invalid_pairing_code,
            main_display,
            new_virtual_display,
            new_virtual_display_sudovda,
            match_viewer_adaptive,
            match_this_display,
            host_default,
            virtual_display_match_viewer,
            virtual_display,
        } = *s;

        vec![
            app_name,
            lan_vpn_chip,
            home_tab,
            new_session,
            close_tab,
            language,
            lang_short,
            lang_switch_tooltip,
            my_devices,
            refresh_my_devices,
            sign_in_to_sync,
            syncing_devices,
            nearby_devices,
            scan_nearby_devices,
            listening_on_lan,
            remote_section,
            nav_connect,
            nav_share,
            account,
            new_account,
            sign_in,
            sign_out,
            refresh,
            scan,
            refresh_network_displays,
            status_connected,
            status_online,
            status_offline,
            status_sharing,
            status_ready,
            status_listening,
            title_share_this_device,
            title_connect_to_device,
            title_create_account,
            title_sign_in_remoters,
            device_fallback,
            host_address,
            pairing_code,
            pairing_code_placeholder,
            display_resolution,
            display_resolution_hint,
            stream_quality,
            connect_btn,
            view_only,
            feature_clipboard,
            feature_drag_downloads,
            feature_adaptive_display,
            feature_lan_vpn,
            codec,
            dynamic_range,
            frame_rate,
            bitrate,
            frame_pacing,
            opt_auto,
            opt_sdr,
            opt_hdr_10bit,
            low_latency,
            balanced,
            local_or_vpn_ip,
            port,
            display,
            virtual_resolution,
            virtual_resolution_hint_win,
            virtual_resolution_hint,
            view_only_hint,
            privacy_screen,
            privacy_screen_hint,
            privacy_screen_active,
            privacy_screen_armed,
            start_sharing,
            invitation,
            discoverable_on_lan,
            waiting_for_guest,
            copy,
            show,
            hide,
            view_only_access,
            full_control_access,
            multi_control_mode,
            control_collaborative,
            control_single,
            control_mode_hint,
            single_controller_access,
            collaborative_control_access,
            stop_sharing,
            sharing_stopped,
            pairing_code_copied,
            address_copied,
            err_select_local_ip,
            err_invalid_port,
            err_select_display,
            sharing_failed_to_start,
            waiting_for_remote_display,
            session_controlling,
            session_connecting,
            session_connected,
            session_viewing,
            session_disconnected,
            control_pill,
            take_control,
            release_input,
            stats,
            quality,
            resolution,
            max_fps,
            pin_stats_overlay,
            hide_stats_overlay,
            gamepad,
            mute_audio,
            unmute_audio,
            muted,
            fullscreen,
            exit_fullscreen,
            disconnect,
            cancel,
            stream,
            live_stats,
            network,
            e2e_latency,
            encode_decode,
            rtt_loss,
            send_files,
            view_only_file_drop_disabled,
            create_account,
            auth_subtitle,
            server_url,
            username,
            username_placeholder,
            display_name,
            display_name_placeholder,
            password,
            creating_account,
            signing_in,
            err_enter_server_url,
            err_enter_username,
            err_enter_password,
            device_removed,
            device_no_longer_listed,
            device_no_longer_listed_hint,
            this_device,
            address,
            no_address_reported,
            active_sharing_session,
            not_currently_sharing,
            remove,
            err_device_offline,
            err_device_not_sharing,
            err_device_no_address,
            err_device_no_pairing_code,
            err_device_invalid_pairing_code,
            main_display,
            new_virtual_display,
            new_virtual_display_sudovda,
            match_viewer_adaptive,
            match_this_display,
            host_default,
            virtual_display_match_viewer,
            virtual_display,
        ]
    }

    #[test]
    fn parses_all_six_locale_codes_and_ignores_posix_defaults() {
        assert_eq!(Locale::parse("en"), Some(Locale::En));
        assert_eq!(Locale::parse("en_US.UTF-8"), Some(Locale::En));
        assert_eq!(Locale::parse("zh"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("zh-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("zh_CN.UTF-8"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("zh-Hans-CN"), Some(Locale::ZhCn));
        assert_eq!(Locale::parse("es"), Some(Locale::Es));
        assert_eq!(Locale::parse("es-ES"), Some(Locale::Es));
        assert_eq!(Locale::parse("es-419"), Some(Locale::Es));
        assert_eq!(Locale::parse("es_MX.UTF-8"), Some(Locale::Es));
        assert_eq!(Locale::parse("fr"), Some(Locale::Fr));
        assert_eq!(Locale::parse("fr-FR"), Some(Locale::Fr));
        assert_eq!(Locale::parse("fr_CA.UTF-8"), Some(Locale::Fr));
        assert_eq!(Locale::parse("ja"), Some(Locale::Ja));
        assert_eq!(Locale::parse("ja-JP"), Some(Locale::Ja));
        assert_eq!(Locale::parse("ja_JP.UTF-8"), Some(Locale::Ja));
        assert_eq!(Locale::parse("ko"), Some(Locale::Ko));
        assert_eq!(Locale::parse("ko-KR"), Some(Locale::Ko));
        assert_eq!(Locale::parse("ko_KR.UTF-8"), Some(Locale::Ko));
        assert_eq!(Locale::parse("C"), None);
        assert_eq!(Locale::parse("C.UTF-8"), None);
        assert_eq!(Locale::parse("POSIX"), None);
        assert_eq!(Locale::parse("de-DE"), None);
        assert_eq!(Locale::parse("   "), None);

        for loc in Locale::ALL {
            assert_eq!(Locale::parse(loc.as_code()), Some(loc));
            assert_eq!(Locale::from_raw(loc.to_raw()), Some(loc));
            assert_eq!(loc.short_label(), strings_for(loc).lang_short);
            assert!(!loc.native_name().trim().is_empty());
        }
    }

    #[test]
    fn detects_locale_with_proper_precedence() {
        assert_eq!(
            detect_locale_from_sources(Some("zh-CN"), Some("en_US.UTF-8"), None, None, None),
            Locale::ZhCn
        );
        assert_eq!(
            detect_locale_from_sources(Some("es-MX"), Some("zh_CN.UTF-8"), None, None, Some("ja")),
            Locale::Es
        );
        assert_eq!(
            detect_locale_from_sources(Some("en"), Some("zh_CN.UTF-8"), None, None, Some("zh")),
            Locale::En
        );
        assert_eq!(
            detect_locale_from_sources(None, Some("fr_FR.UTF-8"), Some("en_US"), None, None),
            Locale::Fr
        );
        assert_eq!(
            detect_locale_from_sources(None, Some("C.UTF-8"), Some("zh_SG.UTF-8"), None, None),
            Locale::ZhCn
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, Some("ko_KR.UTF-8"), Some("en-US")),
            Locale::Ko
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, Some("en_GB.UTF-8"), Some("zh-Hans")),
            Locale::En
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("zh-Hans-CN")),
            Locale::ZhCn
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("es-ES")),
            Locale::Es
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("fr-CA")),
            Locale::Fr
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("ja-JP")),
            Locale::Ja
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("ko-KR")),
            Locale::Ko
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, Some("de-DE")),
            Locale::En
        );
        assert_eq!(
            detect_locale_from_sources(None, None, None, None, None),
            Locale::En
        );
    }

    #[test]
    fn persists_and_reloads_all_six_locales_from_config_file() {
        let unique = format!(
            "remoters-i18n-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let dir = std::env::temp_dir().join(unique);
        let path = dir.join("locale");

        for loc in Locale::ALL {
            save_locale_to_path(&path, loc).expect("save locale");
            assert_eq!(load_locale_from_path(&path), Some(loc));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_six_locale_tables_are_complete_and_distinct() {
        let en = strings_for(Locale::En);
        let mut codes = HashSet::new();
        let mut names = HashSet::new();
        let mut shorts = HashSet::new();
        let mut homes = HashSet::new();

        for loc in Locale::ALL {
            assert!(codes.insert(loc.as_code()), "duplicate code for {loc:?}");
            assert!(
                names.insert(loc.native_name()),
                "duplicate native_name for {loc:?}"
            );
            assert!(
                shorts.insert(loc.short_label()),
                "duplicate short_label for {loc:?}"
            );

            let table = strings_for(loc);
            assert!(
                homes.insert(table.home_tab),
                "duplicate home_tab for {loc:?}"
            );

            for value in all_string_values(table) {
                assert!(
                    !value.trim().is_empty(),
                    "empty string in {loc:?} translation table"
                );
            }

            if loc != Locale::En {
                assert_ne!(table.home_tab, en.home_tab);
                assert_ne!(table.connect_btn, en.connect_btn);
                assert_ne!(table.start_sharing, en.start_sharing);
                assert_ne!(table.stop_sharing, en.stop_sharing);
                assert_ne!(table.disconnect, en.disconnect);
                assert_ne!(table.low_latency, en.low_latency);
                assert_ne!(table.balanced, en.balanced);
                assert_ne!(table.main_display, en.main_display);
                assert_ne!(table.lang_short, en.lang_short);
                assert_ne!(table.privacy_screen, en.privacy_screen);
                assert_ne!(table.privacy_screen_hint, en.privacy_screen_hint);
                assert_ne!(table.privacy_screen_active, en.privacy_screen_active);
                assert_ne!(table.privacy_screen_armed, en.privacy_screen_armed);
                assert_ne!(table.resolution, en.resolution);
                assert_ne!(table.max_fps, en.max_fps);
            }
        }

        let cases = [
            (
                Locale::En,
                "Home",
                "Sharing with 10.0.0.5",
                "3 online",
                "2 connected",
                "2.1 ms · 0 lost",
                "Sending 2 file(s)…",
                "Saved report.pdf to Downloads.",
                "Session ended. timeout",
                "Invalid device address `bad`.",
            ),
            (
                Locale::ZhCn,
                "主页",
                "正在与 10.0.0.5 共享",
                "3 台在线",
                "已连接 2 个",
                "2.1 ms · 丢包 0",
                "正在发送 2 个文件…",
                "已保存 report.pdf 至下载目录。",
                "会话已结束。timeout",
                "无效的设备地址 `bad`。",
            ),
            (
                Locale::Es,
                "Inicio",
                "Compartiendo con 10.0.0.5",
                "3 en línea",
                "2 conectados",
                "2.1 ms · 0 perdidos",
                "Enviando 2 archivos…",
                "report.pdf guardado en Descargas.",
                "Sesión finalizada. timeout",
                "Dirección de dispositivo no válida: `bad`.",
            ),
            (
                Locale::Fr,
                "Accueil",
                "Partage avec 10.0.0.5",
                "3 en ligne",
                "2 connectés",
                "2.1 ms · 0 perdus",
                "Envoi de 2 fichiers…",
                "report.pdf enregistré dans Téléchargements.",
                "Session terminée. timeout",
                "Adresse d’appareil invalide : `bad`.",
            ),
            (
                Locale::Ja,
                "ホーム",
                "10.0.0.5 と共有中",
                "3 台オンライン",
                "2 台接続中",
                "2.1 ms · 損失 0",
                "2 件のファイルを送信中…",
                "report.pdf をダウンロードに保存しました。",
                "セッションが終了しました。timeout",
                "無効なデバイスアドレスです: `bad`。",
            ),
            (
                Locale::Ko,
                "홈",
                "10.0.0.5에 공유 중",
                "3대 온라인",
                "2개 연결됨",
                "2.1 ms · 손실 0",
                "파일 2개 전송 중…",
                "report.pdf 파일을 다운로드 폴더에 저장했습니다.",
                "세션이 종료되었습니다. timeout",
                "유효하지 않은 기기 주소입니다: `bad`.",
            ),
        ];

        for (
            loc,
            expected_home,
            expected_sharing,
            expected_online,
            expected_gamepads,
            expected_rtt,
            expected_sending,
            expected_saved,
            expected_ended,
            expected_invalid_addr,
        ) in cases
        {
            with_locale_for_test(loc, || {
                assert_eq!(locale(), loc);
                assert_eq!(tr().home_tab, expected_home);
                assert_eq!(fmt_sharing_with("10.0.0.5"), expected_sharing);
                assert_eq!(fmt_online_count(3), expected_online);
                assert_eq!(fmt_gamepads_connected(2), expected_gamepads);
                assert_eq!(fmt_rtt_loss("2.1 ms", 0), expected_rtt);
                assert_eq!(fmt_sending_files(2), expected_sending);
                assert_eq!(fmt_saved_file("report.pdf"), expected_saved);
                assert_eq!(fmt_session_ended("timeout"), expected_ended);
                assert_eq!(fmt_invalid_device_address("bad"), expected_invalid_addr);
            });
        }
    }
}
