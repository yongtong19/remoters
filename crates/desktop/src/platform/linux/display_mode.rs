#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct LinuxDisplayMode {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub current: bool,
    pub preferred: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinuxDisplayInfo {
    pub connector: String,
    pub primary: bool,
    pub modes: Vec<LinuxDisplayMode>,
}

fn strip_tree_prefix(line: &str) -> &str {
    line.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─' | '|' | '+' | '-')
    })
}

fn parse_mode_spec_token(token: &str) -> Option<(String, u32, u32, f64)> {
    let trimmed = token.trim_end_matches([',', ':', ';', '*', '+']);
    let (dims, rate_part) = trimmed.split_once('@')?;
    let (w_str, h_str) = dims.split_once('x')?;
    let width = w_str.parse::<u32>().ok()?;
    let height = h_str.parse::<u32>().ok()?;
    let rate_digits: String = rate_part
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let fps = rate_digits.parse::<f64>().ok()?;
    if width < 2 || height < 2 || !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    Some((trimmed.to_owned(), width, height, fps))
}

/// Parses `gdctl show` / `gdctl show -m` output into display connectors and available modes.
pub fn parse_gdctl_show(output: &str) -> Vec<LinuxDisplayInfo> {
    let mut displays: Vec<LinuxDisplayInfo> = Vec::new();
    let mut in_logical = false;
    let mut current_logical_primary = false;
    let mut in_current_mode = false;
    let mut in_logical_monitors_list = false;

    for raw in output.lines() {
        let line = strip_tree_prefix(raw);
        if line.is_empty() {
            continue;
        }
        if line == "Monitors:" && !in_logical {
            in_current_mode = false;
            in_logical_monitors_list = false;
            continue;
        }
        if line == "Monitors:" && in_logical {
            in_logical = false;
            in_current_mode = false;
            in_logical_monitors_list = false;
            continue;
        }
        if line.starts_with("Logical monitors:") {
            in_logical = true;
            in_current_mode = false;
            in_logical_monitors_list = false;
            continue;
        }
        if in_logical {
            if line.starts_with("Logical monitor") {
                current_logical_primary = line.contains("primary");
                in_logical_monitors_list = false;
            } else if let Some(val) = line.strip_prefix("Primary:") {
                current_logical_primary = val.trim().eq_ignore_ascii_case("yes");
            } else if line.starts_with("Monitors:") {
                in_logical_monitors_list = true;
            } else if current_logical_primary && let Some(rest) = line.strip_prefix("Monitor:") {
                let conn = rest.split_whitespace().next().unwrap_or("");
                if let Some(item) = displays.iter_mut().find(|d| d.connector == conn) {
                    item.primary = true;
                }
            } else if current_logical_primary && in_logical_monitors_list {
                let tok = line
                    .split(|c: char| c.is_whitespace() || c == '(')
                    .find(|s| !s.is_empty())
                    .unwrap_or("");
                if let Some(item) = displays.iter_mut().find(|d| d.connector == tok) {
                    item.primary = true;
                }
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("Monitor ") {
            in_current_mode = false;
            let connector = rest
                .split(|c: char| c.is_whitespace() || c == '(')
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_owned();
            if !connector.is_empty() && !displays.iter().any(|d| d.connector == connector) {
                displays.push(LinuxDisplayInfo {
                    connector,
                    primary: false,
                    modes: Vec::new(),
                });
            }
            continue;
        }

        if line.starts_with("Current mode") {
            in_current_mode = true;
            continue;
        }
        if line.starts_with("Modes") || line.starts_with("Preferences") {
            in_current_mode = false;
            continue;
        }

        let Some(active) = displays.last_mut() else {
            continue;
        };
        if let Some((id, width, height, fps)) =
            line.split_whitespace().find_map(parse_mode_spec_token)
        {
            let lower = line.to_ascii_lowercase();
            let is_current = in_current_mode || lower.contains("current");
            let is_preferred = lower.contains("preferred");
            if in_current_mode {
                in_current_mode = false;
            }
            if let Some(existing) = active.modes.iter_mut().find(|m| m.id == id) {
                existing.current |= is_current;
                existing.preferred |= is_preferred;
            } else {
                active.modes.push(LinuxDisplayMode {
                    id,
                    width,
                    height,
                    fps,
                    current: is_current,
                    preferred: is_preferred,
                });
            }
        }
    }

    if !displays.is_empty() && !displays.iter().any(|d| d.primary) {
        displays[0].primary = true;
    }
    displays
}

/// Parses `xrandr --query` output into display connectors and available modes.
pub fn parse_xrandr_query(output: &str) -> Vec<LinuxDisplayInfo> {
    let mut displays: Vec<LinuxDisplayInfo> = Vec::new();
    let mut skip_disconnected = false;

    for line in output.lines() {
        if !line.starts_with(char::is_whitespace) {
            if let Some((connector, rest)) = line.split_once(' ') {
                if rest.starts_with("connected") {
                    skip_disconnected = false;
                    displays.push(LinuxDisplayInfo {
                        connector: connector.to_owned(),
                        primary: rest.contains(" primary "),
                        modes: Vec::new(),
                    });
                } else {
                    skip_disconnected = true;
                }
            }
            continue;
        }
        if skip_disconnected {
            continue;
        }
        let Some(active) = displays.last_mut() else {
            continue;
        };
        let mut parts = line.split_whitespace();
        let Some(mode_name) = parts.next() else {
            continue;
        };
        let Some((w_str, h_str)) = mode_name.split_once('x') else {
            continue;
        };
        let (Ok(width), Ok(height)) = (w_str.parse::<u32>(), h_str.parse::<u32>()) else {
            continue;
        };
        for tok in parts {
            let current = tok.contains('*');
            let preferred = tok.contains('+');
            let clean: String = tok
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(fps) = clean.parse::<f64>()
                && fps.is_finite()
                && fps > 0.0
            {
                active.modes.push(LinuxDisplayMode {
                    id: mode_name.to_owned(),
                    width,
                    height,
                    fps,
                    current,
                    preferred,
                });
            }
        }
    }

    if !displays.is_empty() && !displays.iter().any(|d| d.primary) {
        displays[0].primary = true;
    }
    displays
}

/// Selects the best matching display and mode for `(target_w, target_h, target_fps)`.
pub fn select_best_mode<'a>(
    displays: &'a [LinuxDisplayInfo],
    target_w: u32,
    target_h: u32,
    target_fps: u32,
) -> Option<(&'a LinuxDisplayInfo, &'a LinuxDisplayMode)> {
    let display = displays
        .iter()
        .find(|d| d.primary && !d.modes.is_empty())
        .or_else(|| displays.iter().find(|d| !d.modes.is_empty()))?;
    let want_fps = f64::from(target_fps.max(1));

    let exact = display
        .modes
        .iter()
        .filter(|m| m.width == target_w && m.height == target_h)
        .min_by(|a, b| {
            let da = (a.fps - want_fps).abs();
            let db = (b.fps - want_fps).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
    if let Some(mode) = exact {
        return Some((display, mode));
    }

    let target_area = i64::from(target_w) * i64::from(target_h);
    let best = display.modes.iter().min_by(|a, b| {
        let area_a = i64::from(a.width) * i64::from(a.height);
        let area_b = i64::from(b.width) * i64::from(b.height);
        let diff_a = (area_a - target_area).abs();
        let diff_b = (area_b - target_area).abs();
        diff_a.cmp(&diff_b).then_with(|| {
            let da = (a.fps - want_fps).abs();
            let db = (b.fps - want_fps).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
    })?;
    Some((display, best))
}

#[cfg(target_os = "linux")]
pub fn gdctl_available() -> bool {
    Command::new("gdctl")
        .arg("--help")
        .output()
        .is_ok_and(|out| out.status.success())
}

#[cfg(target_os = "linux")]
pub fn xrandr_available() -> bool {
    Command::new("xrandr")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

#[cfg(target_os = "linux")]
fn run_gdctl_show_combined() -> Option<String> {
    let summary = Command::new("gdctl").arg("show").output().ok()?;
    if !summary.status.success() {
        return None;
    }
    let mut combined = String::from_utf8_lossy(&summary.stdout).into_owned();
    if let Ok(modes) = Command::new("gdctl").args(["show", "-m"]).output()
        && modes.status.success()
    {
        combined.push('\n');
        combined.push_str(&String::from_utf8_lossy(&modes.stdout));
    }
    Some(combined)
}

#[cfg(target_os = "linux")]
pub fn query_linux_displays() -> Vec<LinuxDisplayInfo> {
    if crate::platform::wayland_portal::is_wayland_session()
        && let Some(combined) = run_gdctl_show_combined()
    {
        let parsed = parse_gdctl_show(&combined);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Ok(output) = Command::new("xrandr").arg("--query").output()
        && output.status.success()
    {
        let parsed = parse_xrandr_query(&String::from_utf8_lossy(&output.stdout));
        if !parsed.is_empty() {
            return parsed;
        }
    }
    Vec::new()
}

#[cfg(target_os = "linux")]
pub fn linux_primary_display_spec() -> Option<(String, u32, u32, u16)> {
    let displays = query_linux_displays();
    let display = displays
        .iter()
        .find(|d| d.primary && !d.modes.is_empty())
        .or_else(|| displays.iter().find(|d| !d.modes.is_empty()))?;
    let mode = display
        .modes
        .iter()
        .find(|m| m.current)
        .or_else(|| display.modes.iter().find(|m| m.preferred))
        .or_else(|| display.modes.first())?;
    let fps = (mode.fps.round() as u16).clamp(30, 120);
    Some((display.connector.clone(), mode.width, mode.height, fps))
}

#[cfg(target_os = "linux")]
pub fn set_linux_display_mode(
    width: u32,
    height: u32,
    fps: u32,
) -> Result<Option<(String, String, bool)>> {
    if crate::platform::wayland_portal::is_wayland_session() && gdctl_available() {
        let combined = run_gdctl_show_combined().context("Failed to run gdctl show.")?;
        let displays = parse_gdctl_show(&combined);
        let (display, target_mode) = select_best_mode(&displays, width, height, fps)
            .context("No display modes returned by gdctl.")?;
        let prev_mode = display
            .modes
            .iter()
            .find(|m| m.current)
            .map(|m| m.id.clone());
        if prev_mode.as_deref() == Some(target_mode.id.as_str()) {
            return Ok(None);
        }
        let status = Command::new("gdctl")
            .args([
                "set",
                "--logical-monitor",
                "--primary",
                "--monitor",
                &display.connector,
                "--mode",
                &target_mode.id,
            ])
            .status()
            .context("Failed to invoke gdctl set.")?;
        anyhow::ensure!(
            status.success(),
            "gdctl failed to switch {} to mode {}.",
            display.connector,
            target_mode.id
        );
        return Ok(prev_mode.map(|prev| (display.connector.clone(), prev, true)));
    }

    if xrandr_available() {
        let output = Command::new("xrandr")
            .arg("--query")
            .output()
            .context("Failed to run xrandr --query.")?;
        anyhow::ensure!(output.status.success(), "xrandr --query failed.");
        let displays = parse_xrandr_query(&String::from_utf8_lossy(&output.stdout));
        let (display, target_mode) = select_best_mode(&displays, width, height, fps)
            .context("No display modes returned by xrandr.")?;
        let prev_mode = display
            .modes
            .iter()
            .find(|m| m.current)
            .map(|m| format!("{}@{:.2}", m.id, m.fps));
        let rate_str = format!("{:.2}", target_mode.fps);
        let status = Command::new("xrandr")
            .args([
                "--output",
                &display.connector,
                "--mode",
                &target_mode.id,
                "--rate",
                &rate_str,
            ])
            .status()
            .context("Failed to invoke xrandr --output.")?;
        anyhow::ensure!(
            status.success(),
            "xrandr failed to switch {} to {}@{}Hz.",
            display.connector,
            target_mode.id,
            rate_str
        );
        return Ok(prev_mode.map(|prev| (display.connector.clone(), prev, false)));
    }

    anyhow::bail!("Neither gdctl (Wayland) nor xrandr (X11) is available to resize the display.")
}

#[cfg(target_os = "linux")]
pub fn restore_linux_display_mode(connector: &str, mode_spec: &str, is_gdctl: bool) {
    if is_gdctl {
        let _ = Command::new("gdctl")
            .args([
                "set",
                "--logical-monitor",
                "--primary",
                "--monitor",
                connector,
                "--mode",
                mode_spec,
            ])
            .status();
    } else if let Some((mode_id, rate)) = mode_spec.split_once('@') {
        let _ = Command::new("xrandr")
            .args(["--output", connector, "--mode", mode_id, "--rate", rate])
            .status();
    } else {
        let _ = Command::new("xrandr")
            .args(["--output", connector, "--mode", mode_spec])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_gdctl_show, parse_xrandr_query, select_best_mode};

    #[test]
    fn parses_gdctl_show_and_selects_best_mode() {
        let sample = "\
Monitors:
├──Monitor Virtual-1 (Virtual-1)
│  ├──Modes:
│  │  ├──1280x800@74.990 (1280x800, 74.99Hz, current)
│  │  ├──1920x1080@60.000 (1920x1080, 60.00Hz)
│  │  ├──2560x1440@60.000 (2560x1440, 60.00Hz, preferred)
Logical monitors:
├──Logical monitor (1280x800+0+0, scale 1, primary)
│  └──Monitor: Virtual-1 (Virtual-1)
";
        let displays = parse_gdctl_show(sample);
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].connector, "Virtual-1");
        assert!(displays[0].primary);
        assert_eq!(displays[0].modes.len(), 3);
        assert!(displays[0].modes[0].current);
        assert!(displays[0].modes[2].preferred);

        let (_, picked) = select_best_mode(&displays, 2560, 1440, 60).unwrap();
        assert_eq!(picked.id, "2560x1440@60.000");

        let (_, fallback) = select_best_mode(&displays, 2560, 1600, 60).unwrap();
        assert_eq!(fallback.id, "2560x1440@60.000");

        let gnome50 = "\
Monitors:
└──Monitor Virtual-1 (Red Hat, Inc. 15\")
   ├──Current mode
   │   └──1920x1080@60.000
   └──Preferences
       └──Backlight: None
Monitors:
└──Monitor Virtual-1 (Red Hat, Inc. 15\")
   └──Modes (2)
       ├──3840x2160@60.000
       └──1920x1080@60.000
Logical monitors:
└──Logical monitor #1
   ├──Primary: yes
   └──Monitors: (1)
       └──Virtual-1 (Red Hat, Inc. 15\")
";
        let g50 = parse_gdctl_show(gnome50);
        assert_eq!(g50.len(), 1);
        assert!(g50[0].primary);
        assert!(
            g50[0]
                .modes
                .iter()
                .any(|m| m.id == "1920x1080@60.000" && m.current)
        );
        let (_, g50_4k) = select_best_mode(&g50, 3840, 2160, 60).unwrap();
        assert_eq!(g50_4k.id, "3840x2160@60.000");
    }

    #[test]
    fn parses_xrandr_query_and_selects_exact_mode_rate() {
        let sample = "\
Screen 0: minimum 320 x 200, current 1280 x 800, maximum 8192 x 8192
Virtual-1 connected primary 1280x800+0+0 (normal) 325mm x 203mm
   1280x800      74.99*+
   1920x1080     60.00    50.00
   3840x2160     60.00    50.00
";
        let displays = parse_xrandr_query(sample);
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].connector, "Virtual-1");
        assert!(displays[0].primary);
        let (_, picked) = select_best_mode(&displays, 1920, 1080, 60).unwrap();
        assert_eq!(picked.id, "1920x1080");
        assert!((picked.fps - 60.0).abs() < 0.01);
    }
}
