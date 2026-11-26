use std::net::{Ipv4Addr, SocketAddrV4};

use anyhow::Result;
use gpui::{AssetSource, SharedString};

use crate::{platform::display::DisplaySource, sharing::HostControlMode, ui};

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StartupOptions {
    pub(crate) share: bool,
    pub(crate) bind_ip: Option<Ipv4Addr>,
    pub(crate) bind_port: Option<u16>,
    pub(crate) display: Option<DisplaySource>,
    pub(crate) control_mode: HostControlMode,
    pub(crate) privacy_screen: bool,
    pub(crate) invite_file: Option<std::path::PathBuf>,
    pub(crate) account_server: Option<String>,
}

impl StartupOptions {
    pub(crate) fn from_args() -> Self {
        Self::from_iter(std::env::args().skip(1))
    }

    pub(crate) fn from_iter<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut options = Self::default();
        let mut args = iter.into_iter();
        while let Some(arg) = args.next() {
            let arg = arg.as_ref();
            match arg {
                "--share" => options.share = true,
                "--privacy-screen" => options.privacy_screen = true,
                "--bind" => {
                    if let Some(value) = args.next() {
                        let value = value.as_ref();
                        if let Ok(addr) = value.parse::<SocketAddrV4>() {
                            options.bind_ip = Some(*addr.ip());
                            options.bind_port = Some(addr.port());
                        } else if let Ok(ip) = value.parse::<Ipv4Addr>() {
                            options.bind_ip = Some(ip);
                        }
                    }
                }
                "--display" => {
                    if let Some(value) = args.next() {
                        options.display = match value.as_ref() {
                            "primary" => Some(DisplaySource::Primary),
                            "virtual" => Some(DisplaySource::Virtual),
                            _ => None,
                        };
                    }
                }
                "--control-mode" => {
                    if let Some(value) = args.next() {
                        options.control_mode = match value.as_ref() {
                            "single" | "single-controller" => HostControlMode::SingleController,
                            "collaborative" | "simultaneous" => HostControlMode::Collaborative,
                            _ => options.control_mode,
                        };
                    }
                }
                "--invite-file" => {
                    options.invite_file = args.next().map(|v| std::path::PathBuf::from(v.as_ref()));
                }
                "--account-server" => {
                    options.account_server = args.next().map(|v| v.as_ref().to_owned());
                }
                _ if arg.starts_with("--account-server=") => {
                    options.account_server =
                        arg.strip_prefix("--account-server=").map(ToOwned::to_owned);
                }
                _ => {}
            }
        }
        options
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::StartupOptions;
    use crate::{platform::display::DisplaySource, sharing::HostControlMode};

    #[test]
    fn startup_options_defaults_and_parses_privacy_screen_flag() {
        let defaults = StartupOptions::from_iter(std::iter::empty::<&str>());
        assert!(!defaults.share);
        assert!(!defaults.privacy_screen);
        assert_eq!(defaults.control_mode, HostControlMode::Collaborative);
        assert_eq!(defaults.display, None);

        let parsed = StartupOptions::from_iter([
            "--share",
            "--privacy-screen",
            "--display",
            "virtual",
            "--control-mode",
            "single",
            "--bind",
            "192.168.1.50:48000",
        ]);
        assert!(parsed.share);
        assert!(parsed.privacy_screen);
        assert_eq!(parsed.display, Some(DisplaySource::Virtual));
        assert_eq!(parsed.control_mode, HostControlMode::SingleController);
        assert_eq!(parsed.bind_ip, Some(Ipv4Addr::new(192, 168, 1, 50)));
        assert_eq!(parsed.bind_port, Some(48000));
    }
}

pub(crate) struct DesktopAssets;

impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if path == ui::REFRESH_ICON_PATH {
            return Ok(Some(std::borrow::Cow::Borrowed(
                ui::REFRESH_CW_SVG.as_bytes(),
            )));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut items = gpui_component_assets::Assets.list(path)?;
        if path.is_empty() || path == "icons" {
            let custom: SharedString = ui::REFRESH_ICON_PATH.into();
            if !items.contains(&custom) {
                items.push(custom);
            }
        }
        Ok(items)
    }
}
