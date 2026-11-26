use anyhow::{Context, Result};
use gpui::SharedString;
use gpui_component::select::SelectItem;
use remoters_core::session::DisplayPreference;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::{ffi::c_void, ptr::NonNull};
#[cfg(target_os = "macos")]
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use xcap::Monitor;

use super::primary_monitor;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl VirtualSpec {
    pub const DEFAULT_FALLBACK: Self = Self {
        width: 1920,
        height: 1080,
        fps: 60,
    };

    pub fn sanitized(width: u32, height: u32, fps: u32) -> Result<Self> {
        let format =
            crate::codec::virtual_format(width, height, fps.min(u32::from(u16::MAX)) as u16)?;
        Ok(Self {
            width: u32::from(format.width),
            height: u32::from(format.height),
            fps: u32::from(format.frames_per_second),
        })
    }

    pub fn from_preference(pref: DisplayPreference) -> Self {
        Self::sanitized(
            u32::from(pref.width),
            u32::from(pref.height),
            u32::from(pref.frames_per_second),
        )
        .unwrap_or(Self::DEFAULT_FALLBACK)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualResolution {
    Adaptive,
    Fixed(VirtualSpec),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySource {
    Primary,
    Existing(u32),
    Virtual,
}

#[derive(Clone)]
enum DisplayChoiceLabel {
    Primary(Option<(u32, u32)>),
    Existing(SharedString),
    Virtual,
}

#[derive(Clone)]
pub struct DisplayChoice {
    pub source: DisplaySource,
    label: DisplayChoiceLabel,
}

impl SelectItem for DisplayChoice {
    type Value = DisplaySource;

    fn title(&self) -> SharedString {
        let s = crate::i18n::tr();
        match &self.label {
            DisplayChoiceLabel::Primary(Some((w, h))) => {
                crate::i18n::fmt_main_display_size(*w, *h).into()
            }
            DisplayChoiceLabel::Primary(None) => s.main_display.into(),
            DisplayChoiceLabel::Existing(title) => title.clone(),
            DisplayChoiceLabel::Virtual => {
                if cfg!(target_os = "windows") {
                    s.new_virtual_display_sudovda.into()
                } else {
                    s.new_virtual_display.into()
                }
            }
        }
    }

    fn value(&self) -> &DisplaySource {
        &self.source
    }
}

pub fn display_choices() -> Vec<DisplayChoice> {
    #[cfg(target_os = "linux")]
    if super::wayland_portal::is_wayland_session() {
        let dims = super::linux::linux_primary_display_spec().map(|(_, w, h, _)| (w, h));
        return vec![
            DisplayChoice {
                source: DisplaySource::Primary,
                label: DisplayChoiceLabel::Primary(dims),
            },
            DisplayChoice {
                source: DisplaySource::Virtual,
                label: DisplayChoiceLabel::Virtual,
            },
        ];
    }
    let mut choices = vec![DisplayChoice {
        source: DisplaySource::Primary,
        label: DisplayChoiceLabel::Primary(None),
    }];
    if let Ok(monitors) = Monitor::all() {
        let primary_id = monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .and_then(|monitor| monitor.id().ok());
        for monitor in monitors {
            if primary_id.is_some() && monitor.id().ok() == primary_id {
                continue;
            }
            let entry = (|| -> Result<_> {
                let (width, height) = crate::codec::capture_dimensions(&monitor)?;
                Ok(DisplayChoice {
                    source: DisplaySource::Existing(monitor.id()?),
                    label: DisplayChoiceLabel::Existing(
                        format!("{} · {width} × {height}", monitor.name()?).into(),
                    ),
                })
            })();
            if let Ok(entry) = entry {
                choices.push(entry);
            }
        }
    }
    choices.push(DisplayChoice {
        source: DisplaySource::Virtual,
        label: DisplayChoiceLabel::Virtual,
    });
    choices
}

#[derive(Clone)]
pub struct ResolutionChoice {
    pub value: VirtualResolution,
    title: SharedString,
}

impl SelectItem for ResolutionChoice {
    type Value = VirtualResolution;

    fn title(&self) -> SharedString {
        match self.value {
            VirtualResolution::Adaptive => crate::i18n::tr().match_viewer_adaptive.into(),
            VirtualResolution::Fixed(_) => self.title.clone(),
        }
    }

    fn value(&self) -> &VirtualResolution {
        &self.value
    }
}

const PRESET_SPECS: [(&str, VirtualSpec); 4] = [
    (
        "1920 × 1080 · 60 Hz",
        VirtualSpec {
            width: 1920,
            height: 1080,
            fps: 60,
        },
    ),
    (
        "2560 × 1440 · 60 Hz",
        VirtualSpec {
            width: 2560,
            height: 1440,
            fps: 60,
        },
    ),
    (
        "2560 × 1600 · 60 Hz",
        VirtualSpec {
            width: 2560,
            height: 1600,
            fps: 60,
        },
    ),
    (
        "3840 × 2160 · 60 Hz (4K)",
        VirtualSpec {
            width: 3840,
            height: 2160,
            fps: 60,
        },
    ),
];

pub fn virtual_resolution_choices() -> Vec<ResolutionChoice> {
    let mut choices = vec![ResolutionChoice {
        value: VirtualResolution::Adaptive,
        title: "Match viewer (adaptive)".into(),
    }];
    choices.extend(
        PRESET_SPECS
            .into_iter()
            .map(|(title, spec)| ResolutionChoice {
                value: VirtualResolution::Fixed(spec),
                title: title.into(),
            }),
    );
    choices
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewerResolution {
    MatchLocal,
    Fixed(VirtualSpec),
    HostDefault,
}

#[derive(Clone)]
enum ViewerResolutionLabel {
    MatchLocal(Option<(u16, u16)>),
    Fixed(SharedString),
    HostDefault,
}

#[derive(Clone)]
pub struct ViewerResolutionChoice {
    pub value: ViewerResolution,
    label: ViewerResolutionLabel,
}

impl SelectItem for ViewerResolutionChoice {
    type Value = ViewerResolution;

    fn title(&self) -> SharedString {
        let s = crate::i18n::tr();
        match &self.label {
            ViewerResolutionLabel::MatchLocal(Some((w, h))) => {
                crate::i18n::fmt_match_this_display_size(*w, *h).into()
            }
            ViewerResolutionLabel::MatchLocal(None) => s.match_this_display.into(),
            ViewerResolutionLabel::Fixed(title) => title.clone(),
            ViewerResolutionLabel::HostDefault => s.host_default.into(),
        }
    }

    fn value(&self) -> &ViewerResolution {
        &self.value
    }
}

pub fn local_display_preference() -> Option<DisplayPreference> {
    #[cfg(target_os = "macos")]
    {
        let mut display_id = 0;
        let mut width = 0;
        let mut height = 0;
        if unsafe { remoters_macos_fullscreen_pixels(&mut display_id, &mut width, &mut height) } {
            let monitor = monitor_by_id(display_id)
                .ok()
                .or_else(|| primary_monitor().ok());
            let fps = monitor
                .and_then(|monitor| match monitor.frequency() {
                    Ok(hz) if hz.is_finite() && hz >= 30.0 => {
                        Some((hz.round() as u16).clamp(30, 120))
                    }
                    _ => None,
                })
                .unwrap_or(60);
            if let Ok(pref) = DisplayPreference::normalized(width, height, fps) {
                return Some(pref);
            }
        }
    }
    #[cfg(target_os = "linux")]
    if super::wayland_portal::is_wayland_session()
        && let Some((_, width, height, fps)) = super::linux::linux_primary_display_spec()
    {
        return DisplayPreference::normalized(width, height, fps).ok();
    }
    let monitor = primary_monitor().ok()?;
    let (width, height) = crate::codec::capture_dimensions(&monitor).ok()?;
    let fps = match monitor.frequency() {
        Ok(hz) if hz.is_finite() && hz >= 30.0 => (hz.round() as u16).clamp(30, 120),
        _ => 60,
    };
    DisplayPreference::normalized(width, height, fps).ok()
}

pub fn viewer_resolution_choices() -> Vec<ViewerResolutionChoice> {
    let local_dims = local_display_preference().map(|pref| (pref.width, pref.height));
    let mut choices = vec![ViewerResolutionChoice {
        value: ViewerResolution::MatchLocal,
        label: ViewerResolutionLabel::MatchLocal(local_dims),
    }];
    choices.extend(
        PRESET_SPECS
            .into_iter()
            .map(|(title, spec)| ViewerResolutionChoice {
                value: ViewerResolution::Fixed(spec),
                label: ViewerResolutionLabel::Fixed(title.into()),
            }),
    );
    choices.push(ViewerResolutionChoice {
        value: ViewerResolution::HostDefault,
        label: ViewerResolutionLabel::HostDefault,
    });
    choices
}

pub fn resolve_viewer_preference(choice: ViewerResolution) -> Option<DisplayPreference> {
    match choice {
        ViewerResolution::MatchLocal => local_display_preference(),
        ViewerResolution::Fixed(spec) => {
            DisplayPreference::normalized(spec.width, spec.height, spec.fps.clamp(1, 120) as u16)
                .ok()
        }
        ViewerResolution::HostDefault => None,
    }
}

pub fn monitor_by_id(id: u32) -> Result<Monitor> {
    Monitor::all()?
        .into_iter()
        .find(|monitor| monitor.id().ok() == Some(id))
        .context("The selected display is no longer connected.")
}

pub fn open_existing(source: DisplaySource) -> Result<Monitor> {
    match source {
        DisplaySource::Primary => primary_monitor(),
        DisplaySource::Existing(id) => monitor_by_id(id),
        DisplaySource::Virtual => {
            anyhow::bail!("Virtual displays must be created before opening.")
        }
    }
}

#[cfg(target_os = "macos")]
pub fn virtual_display_supported() -> Result<()> {
    anyhow::ensure!(
        unsafe { remoters_virtual_display_available() },
        "macOS virtual display support (CGVirtualDisplay) is unavailable on this system."
    );
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn virtual_display_supported() -> Result<()> {
    sudovda_status(
        unsafe { remoters_virtual_display_available() },
        "Windows could not access the SudoVDA virtual display driver.",
    )
}

#[cfg(target_os = "linux")]
pub fn virtual_display_supported() -> Result<()> {
    if super::wayland_portal::is_wayland_session() {
        anyhow::ensure!(
            super::linux::gdctl_available(),
            "GNOME display tool (gdctl) is unavailable on this Wayland session."
        );
        return Ok(());
    }
    anyhow::ensure!(
        super::linux::xrandr_available(),
        "xrandr is unavailable on this X11 session."
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn virtual_display_supported() -> Result<()> {
    anyhow::bail!("Virtual displays are only supported on macOS, Windows, and Linux.")
}

const MAX_VIRTUAL_REFRESH_HZ: u32 = 120;
const DOUBLE_REFRESH_THRESHOLD_FPS: u32 = 60;

/// Computes the Apollo-style virtual display refresh rate for a stream target `fps`.
///
/// Streams running at `<= 60 fps` request `min(fps * 2, 120)` Hz to halve average
/// compositor capture phase latency while encoding/streaming stays at `stream_fps`.
pub fn double_refresh_rate(stream_fps: u32) -> u32 {
    if stream_fps <= DOUBLE_REFRESH_THRESHOLD_FPS {
        stream_fps.saturating_mul(2).min(MAX_VIRTUAL_REFRESH_HZ)
    } else {
        stream_fps.min(MAX_VIRTUAL_REFRESH_HZ)
    }
}

/// Returns virtual display specs in attempt order: doubled refresh rate first (when
/// `spec.fps <= 60`), followed by the original `spec` as automatic fallback.
pub fn virtual_display_refresh_candidates(spec: VirtualSpec) -> Vec<VirtualSpec> {
    let doubled_fps = double_refresh_rate(spec.fps);
    if doubled_fps > spec.fps {
        vec![
            VirtualSpec {
                fps: doubled_fps,
                ..spec
            },
            spec,
        ]
    } else {
        vec![spec]
    }
}

fn try_create_with_double_refresh<T, F>(
    spec: VirtualSpec,
    mut create: F,
) -> Result<(T, VirtualSpec)>
where
    F: FnMut(VirtualSpec) -> Result<T>,
{
    let candidates = virtual_display_refresh_candidates(spec);
    let mut last_error = None;
    for candidate in candidates {
        match create(candidate) {
            Ok(created) => return Ok((created, candidate)),
            Err(error) => {
                if candidate.fps != spec.fps {
                    eprintln!(
                        "Virtual display {}x{}@{}Hz unavailable ({error:#}); falling back to {}Hz",
                        candidate.width, candidate.height, candidate.fps, spec.fps
                    );
                }
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Could not create virtual display.")))
}

pub fn create_virtual(spec: VirtualSpec) -> Result<(Monitor, VirtualDisplay)> {
    let spec = VirtualSpec::sanitized(spec.width, spec.height, spec.fps)?;
    let ((display, id), _applied) = try_create_with_double_refresh(spec, VirtualDisplay::new)?;
    Ok((monitor_by_id(id)?, display))
}

#[cfg(target_os = "linux")]
pub fn apply_linux_virtual_mode(spec: VirtualSpec) -> Result<(VirtualDisplay, u32, u32, u16)> {
    let spec = VirtualSpec::sanitized(spec.width, spec.height, spec.fps)?;
    let (display, _applied) = try_create_with_double_refresh(spec, VirtualDisplay::new_mode)?;
    let (_, w, h, actual_fps) = super::linux::linux_primary_display_spec().unwrap_or_else(|| {
        (
            "Virtual-1".to_owned(),
            spec.width,
            spec.height,
            spec.fps as u16,
        )
    });
    // Keep streaming/encoding at target `spec.fps` even when the virtual display runs at 2x Hz.
    let stream_fps = actual_fps.min(spec.fps as u16);
    Ok((display, w, h, stream_fps))
}

#[cfg(target_os = "windows")]
pub struct VirtualDisplay(NonNull<c_void>);

// `windows_display.cpp`'s `VirtualDisplay` owns Win32 handles and a worker thread
// without thread affinity, so moving ownership across threads is sound.
#[cfg(target_os = "windows")]
unsafe impl Send for VirtualDisplay {}

#[cfg(target_os = "macos")]
pub struct VirtualDisplay(Child);

#[cfg(target_os = "linux")]
pub struct VirtualDisplay {
    restore: Option<(String, String, bool)>,
}

#[cfg(target_os = "linux")]
impl VirtualDisplay {
    fn new_mode(spec: VirtualSpec) -> Result<Self> {
        let restore = super::linux::set_linux_display_mode(spec.width, spec.height, spec.fps)?;
        Ok(Self { restore })
    }

    fn new(spec: VirtualSpec) -> Result<(Self, u32)> {
        let display = Self::new_mode(spec)?;
        let monitor = primary_monitor()?;
        Ok((display, monitor.id()?))
    }
}

#[cfg(target_os = "linux")]
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        if let Some((connector, mode_spec, is_gdctl)) = self.restore.take() {
            super::linux::restore_linux_display_mode(&connector, &mode_spec, is_gdctl);
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub struct VirtualDisplay;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl VirtualDisplay {
    fn new(_spec: VirtualSpec) -> Result<(Self, u32)> {
        virtual_display_supported()?;
        unreachable!()
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn remoters_macos_fullscreen_pixels(
        display_id: *mut u32,
        width: *mut u32,
        height: *mut u32,
    ) -> bool;
    #[cfg(target_os = "macos")]
    fn remoters_virtual_display_available() -> bool;
    #[cfg(target_os = "macos")]
    fn remoters_virtual_display_create(
        width: u32,
        height: u32,
        fps: u32,
        id: *mut u32,
    ) -> *mut c_void;
    #[cfg(target_os = "windows")]
    fn remoters_virtual_display_available() -> u32;
    #[cfg(target_os = "windows")]
    fn remoters_virtual_display_create(
        width: u32,
        height: u32,
        fps: u32,
        id: *mut u32,
        display: *mut *mut c_void,
    ) -> u32;
    fn remoters_virtual_display_destroy(display: *mut c_void);
}

#[cfg(target_os = "windows")]
fn sudovda_status(status: u32, context: &'static str) -> Result<()> {
    match status {
        0 => Ok(()),
        1167 => anyhow::bail!(
            "Creating a virtual display requires the SudoVDA driver. Install SudoVDA or choose an existing display."
        ),
        1306 => anyhow::bail!(
            "The installed SudoVDA driver version is too old; SudoVDA 0.2 or newer is required."
        ),
        1460 => {
            anyhow::bail!("Windows did not activate the virtual display as an extended screen.")
        }
        other => Err(std::io::Error::from_raw_os_error(other as i32)).context(context),
    }
}

#[cfg(target_os = "windows")]
impl VirtualDisplay {
    fn new(spec: VirtualSpec) -> Result<(Self, u32)> {
        let mut id = 0;
        let mut display = std::ptr::null_mut();
        let status = unsafe {
            remoters_virtual_display_create(
                spec.width,
                spec.height,
                spec.fps,
                &mut id,
                &mut display,
            )
        };
        sudovda_status(
            status,
            "Windows could not create the SudoVDA virtual display.",
        )?;
        let display =
            NonNull::new(display).context("The SudoVDA display handle is unavailable.")?;
        Ok((Self(display), id))
    }
}

#[cfg(target_os = "windows")]
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        unsafe { remoters_virtual_display_destroy(self.0.as_ptr()) }
    }
}

#[cfg(target_os = "macos")]
impl VirtualDisplay {
    fn new(spec: VirtualSpec) -> Result<(Self, u32)> {
        // WindowServer discards the display when its creating process exits, even
        // on systems that retain a display after the selected mode was changed.
        let mut worker = Self(
            Command::new(std::env::current_exe()?)
                .arg("--virtual-display-worker")
                .arg(spec.width.to_string())
                .arg(spec.height.to_string())
                .arg(spec.fps.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let output = worker
            .0
            .stdout
            .take()
            .context("Missing virtual display response.")?;
        let (answer, ready) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("virtual-display-ready".into())
            .spawn(move || {
                let mut line = String::new();
                let response = BufReader::new(output).read_line(&mut line).map(|_| line);
                let _ = answer.send(response);
            })?;
        let id = ready
            .recv_timeout(Duration::from_secs(4))
            .context("macOS did not activate the virtual display.")??
            .trim()
            .parse()
            .context("macOS could not activate the virtual display.")?;
        Ok((worker, id))
    }
}

#[cfg(target_os = "macos")]
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        drop(self.0.stdin.take());
        let deadline = Instant::now() + Duration::from_millis(200);
        while let Ok(None) = self.0.try_wait() {
            if Instant::now() >= deadline {
                let _ = self.0.kill();
                let _ = self.0.wait();
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(target_os = "macos")]
pub fn virtual_display_worker() -> Result<()> {
    let mut args = std::env::args().skip(2);
    let spec = match (args.next(), args.next(), args.next()) {
        (Some(width), Some(height), Some(fps)) => VirtualSpec::sanitized(
            width.parse().context("Invalid virtual display width.")?,
            height.parse().context("Invalid virtual display height.")?,
            fps.parse()
                .context("Invalid virtual display refresh rate.")?,
        )?,
        (None, None, None) => VirtualSpec {
            width: 3840,
            height: 2160,
            fps: 60,
        },
        _ => anyhow::bail!("Expected --virtual-display-worker [<width> <height> <fps>]."),
    };
    let mut id = 0;
    let display =
        unsafe { remoters_virtual_display_create(spec.width, spec.height, spec.fps, &mut id) };
    let display = NonNull::new(display).context("macOS could not set the virtual display mode.")?;
    let result = (|| -> Result<()> {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{id}")?;
        output.flush()?;
        // The parent owns the other end of this pipe.
        std::io::stdin().lock().read_line(&mut String::new())?;
        Ok(())
    })();
    unsafe { remoters_virtual_display_destroy(display.as_ptr()) };
    result
}

#[cfg(test)]
mod tests {
    use super::{
        ViewerResolution, VirtualSpec, double_refresh_rate, resolve_viewer_preference,
        try_create_with_double_refresh, virtual_display_refresh_candidates,
    };
    use remoters_core::session::DisplayPreference;

    #[test]
    fn sanitized_clamps_scales_and_aligns_virtual_specs() {
        assert_eq!(
            VirtualSpec::sanitized(1921, 1081, 24).unwrap(),
            VirtualSpec {
                width: 1920,
                height: 1080,
                fps: 30,
            }
        );
        assert_eq!(
            VirtualSpec::sanitized(5120, 2880, 144).unwrap(),
            VirtualSpec {
                width: 3840,
                height: 2160,
                fps: 120,
            }
        );
        assert!(VirtualSpec::sanitized(0, 1080, 60).is_err());
        assert!(VirtualSpec::sanitized(1920, 1, 60).is_err());
    }

    #[test]
    fn from_preference_preserves_valid_and_normalizes_bounds() {
        let pref = DisplayPreference {
            width: 2560,
            height: 1600,
            frames_per_second: 60,
        };
        assert_eq!(
            VirtualSpec::from_preference(pref),
            VirtualSpec {
                width: 2560,
                height: 1600,
                fps: 60,
            }
        );

        let low_fps = DisplayPreference {
            width: 1920,
            height: 1080,
            frames_per_second: 15,
        };
        assert_eq!(
            VirtualSpec::from_preference(low_fps),
            VirtualSpec {
                width: 1920,
                height: 1080,
                fps: 30,
            }
        );

        let invalid = DisplayPreference {
            width: 0,
            height: 0,
            frames_per_second: 60,
        };
        assert_eq!(
            VirtualSpec::from_preference(invalid),
            VirtualSpec::DEFAULT_FALLBACK
        );
    }

    #[test]
    fn sanitized_and_from_preference_preserve_non_16_9_mobile_resolutions() {
        for (width, height, fps) in [
            (2400, 1080, 60),
            (2640, 1200, 90),
            (2772, 1240, 120),
            (2560, 1152, 60),
            (3168, 1440, 120),
            (1080, 2400, 60),
        ] {
            assert_eq!(
                VirtualSpec::sanitized(width, height, fps).unwrap(),
                VirtualSpec { width, height, fps }
            );
            let pref = DisplayPreference::normalized(width, height, fps as u16).unwrap();
            assert_eq!(
                VirtualSpec::from_preference(pref),
                VirtualSpec { width, height, fps }
            );
        }

        // Odd non-16:9 dimensions align down to even pixel dimensions cleanly.
        assert_eq!(
            VirtualSpec::sanitized(2773, 1241, 60).unwrap(),
            VirtualSpec {
                width: 2772,
                height: 1240,
                fps: 60,
            }
        );

        // 2x virtual display refresh rate preserves exact non-16:9 resolution.
        let mobile_60 = VirtualSpec {
            width: 2640,
            height: 1200,
            fps: 60,
        };
        assert_eq!(
            virtual_display_refresh_candidates(mobile_60),
            vec![
                VirtualSpec {
                    width: 2640,
                    height: 1200,
                    fps: 120,
                },
                mobile_60,
            ]
        );
    }

    #[test]
    fn resolve_viewer_preference_maps_fixed_and_host_default() {
        assert_eq!(
            resolve_viewer_preference(ViewerResolution::HostDefault),
            None
        );
        assert_eq!(
            resolve_viewer_preference(ViewerResolution::Fixed(VirtualSpec {
                width: 2560,
                height: 1440,
                fps: 60,
            })),
            Some(DisplayPreference {
                width: 2560,
                height: 1440,
                frames_per_second: 60,
            })
        );
        assert_eq!(
            resolve_viewer_preference(ViewerResolution::Fixed(VirtualSpec {
                width: 5120,
                height: 2880,
                fps: 144,
            })),
            Some(DisplayPreference {
                width: 3840,
                height: 2160,
                frames_per_second: 120,
            })
        );
        assert_eq!(
            DisplayPreference::normalized(3024, 1900, 120).unwrap(),
            DisplayPreference {
                width: 3024,
                height: 1900,
                frames_per_second: 120,
            }
        );
    }

    #[test]
    fn double_refresh_rate_doubles_up_to_60fps_and_clamps_to_120hz() {
        assert_eq!(double_refresh_rate(30), 60);
        assert_eq!(double_refresh_rate(45), 90);
        assert_eq!(double_refresh_rate(60), 120);
        assert_eq!(double_refresh_rate(61), 61);
        assert_eq!(double_refresh_rate(90), 90);
        assert_eq!(double_refresh_rate(120), 120);
        assert_eq!(double_refresh_rate(144), 120);

        let spec60 = VirtualSpec {
            width: 1920,
            height: 1080,
            fps: 60,
        };
        assert_eq!(
            virtual_display_refresh_candidates(spec60),
            vec![
                VirtualSpec {
                    width: 1920,
                    height: 1080,
                    fps: 120,
                },
                spec60,
            ]
        );

        let spec90 = VirtualSpec {
            width: 2560,
            height: 1440,
            fps: 90,
        };
        assert_eq!(virtual_display_refresh_candidates(spec90), vec![spec90]);
    }

    #[test]
    fn try_create_with_double_refresh_uses_doubled_rate_and_falls_back_cleanly() {
        let spec60 = VirtualSpec {
            width: 1920,
            height: 1080,
            fps: 60,
        };

        // Case 1: platform supports 120Hz -> picks 120Hz on first attempt.
        let mut attempts = Vec::new();
        let (handle, applied) = try_create_with_double_refresh(spec60, |candidate| {
            attempts.push(candidate.fps);
            Ok(candidate.fps)
        })
        .expect("120Hz virtual display should succeed");
        assert_eq!(attempts, vec![120]);
        assert_eq!(handle, 120);
        assert_eq!(applied.fps, 120);

        // Case 2: platform rejects 120Hz -> transparently falls back to 60Hz.
        let mut fallback_attempts = Vec::new();
        let (handle, applied) = try_create_with_double_refresh(spec60, |candidate| {
            fallback_attempts.push(candidate.fps);
            if candidate.fps > 60 {
                anyhow::bail!("120Hz unsupported by driver");
            }
            Ok(candidate.fps)
        })
        .expect("60Hz fallback should succeed");
        assert_eq!(fallback_attempts, vec![120, 60]);
        assert_eq!(handle, 60);
        assert_eq!(applied.fps, 60);

        // Case 3: both fail -> returns error.
        let err = try_create_with_double_refresh::<(), _>(spec60, |_| {
            anyhow::bail!("virtual display unavailable")
        })
        .unwrap_err();
        assert!(err.to_string().contains("virtual display unavailable"));
    }

    #[test]
    fn dropdown_choice_titles_follow_active_locale() {
        use gpui_component::select::SelectItem;

        let virtual_res = super::virtual_resolution_choices();
        let adaptive = virtual_res.first().expect("adaptive resolution choice");
        let viewer_res = super::viewer_resolution_choices();
        let host_default = viewer_res.last().expect("host default resolution choice");

        crate::i18n::with_locale_for_test(crate::i18n::Locale::En, || {
            assert_eq!(adaptive.title().as_ref(), "Match viewer (adaptive)");
            assert_eq!(host_default.title().as_ref(), "Host default");
        });
        crate::i18n::with_locale_for_test(crate::i18n::Locale::ZhCn, || {
            assert_eq!(adaptive.title().as_ref(), "匹配来宾 (自适应)");
            assert_eq!(host_default.title().as_ref(), "主机默认");
        });
    }
}
