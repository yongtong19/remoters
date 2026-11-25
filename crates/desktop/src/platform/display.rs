use anyhow::{Context, Result};
use gpui::SharedString;
use gpui_component::select::SelectItem;
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

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DisplaySource {
    Primary,
    Existing(u32),
    Virtual4k,
}

#[derive(Clone)]
pub struct DisplayChoice {
    pub source: DisplaySource,
    title: SharedString,
}

impl SelectItem for DisplayChoice {
    type Value = DisplaySource;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &DisplaySource {
        &self.source
    }
}

pub fn display_choices() -> Vec<DisplayChoice> {
    let mut choices = vec![DisplayChoice {
        source: DisplaySource::Primary,
        title: "Main display".into(),
    }];
    if let Ok(monitors) = Monitor::all() {
        for monitor in monitors {
            if monitor.is_primary().unwrap_or(false) {
                continue;
            }
            let entry = (|| -> Result<_> {
                let (width, height) = crate::codec::capture_dimensions(&monitor)?;
                Ok(DisplayChoice {
                    source: DisplaySource::Existing(monitor.id()?),
                    title: format!("{} · {width} × {height}", monitor.name()?).into(),
                })
            })();
            if let Ok(entry) = entry {
                choices.push(entry);
            }
        }
    }
    choices.push(DisplayChoice {
        source: DisplaySource::Virtual4k,
        title: "New virtual display · 4K".into(),
    });
    choices
}

pub fn monitor_by_id(id: u32) -> Result<Monitor> {
    Monitor::all()?
        .into_iter()
        .find(|monitor| monitor.id().ok() == Some(id))
        .context("The selected display is no longer connected.")
}

pub fn open(source: DisplaySource) -> Result<(Monitor, Option<VirtualDisplay>)> {
    match source {
        DisplaySource::Primary => Ok((primary_monitor()?, None)),
        DisplaySource::Existing(id) => Ok((monitor_by_id(id)?, None)),
        DisplaySource::Virtual4k => {
            let (display, id) = VirtualDisplay::new()?;
            Ok((monitor_by_id(id)?, Some(display)))
        }
    }
}

#[cfg(target_os = "windows")]
pub struct VirtualDisplay(NonNull<c_void>);

#[cfg(target_os = "macos")]
pub struct VirtualDisplay(Child);

unsafe extern "C" {
    #[cfg(target_os = "macos")]
    fn remoters_virtual_display_create(
        width: u32,
        height: u32,
        fps: u32,
        id: *mut u32,
    ) -> *mut c_void;
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
impl VirtualDisplay {
    fn new() -> Result<(Self, u32)> {
        let mut id = 0;
        let mut display = std::ptr::null_mut();
        let status =
            unsafe { remoters_virtual_display_create(3840, 2160, 60, &mut id, &mut display) };
        match status {
            0 => {}
            1167 => anyhow::bail!("Creating a virtual display requires the SudoVDA driver."),
            1460 => {
                anyhow::bail!("Windows did not activate the virtual display as an extended screen.")
            }
            other => {
                return Err(std::io::Error::from_raw_os_error(other as i32))
                    .context("Windows could not create the SudoVDA virtual display.");
            }
        }
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
    fn new() -> Result<(Self, u32)> {
        // WindowServer discards the display when its creating process exits, even
        // on systems that retain a display after the selected mode was changed.
        let mut worker = Self(
            Command::new(std::env::current_exe()?)
                .arg("--virtual-display-worker")
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
            .recv_timeout(Duration::from_secs(3))
            .context("macOS did not activate the virtual display.")??
            .trim()
            .parse()
            .context("macOS could not activate a 4K virtual display.")?;
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
    let mut id = 0;
    let display = unsafe { remoters_virtual_display_create(3840, 2160, 60, &mut id) };
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
