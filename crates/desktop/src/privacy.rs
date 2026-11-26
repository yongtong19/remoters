use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

/// Describes the display currently being streamed so the privacy screen covers physical monitors
/// without interfering with remote capture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrivacyTarget {
    pub is_virtual_display: bool,
    pub captured_display_id: Option<u32>,
}

/// Rectangle in virtual desktop coordinates, used for selecting physical monitors to curtain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(C)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl MonitorRect {
    #[allow(dead_code)]
    pub fn width(self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    #[allow(dead_code)]
    pub fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }
}

/// Selects which Windows monitors should receive fullscreen black privacy curtains and whether
/// `WDA_EXCLUDEFROMCAPTURE` should be applied to each curtain window.
#[allow(dead_code)]
pub(crate) fn select_windows_curtain_targets(
    monitors: &[(u32, MonitorRect)],
    target: PrivacyTarget,
) -> Vec<(MonitorRect, bool)> {
    monitors
        .iter()
        .filter(|&&(id, rect)| {
            if rect.width() <= 0 || rect.height() <= 0 {
                return false;
            }
            if target.is_virtual_display
                && let Some(captured_id) = target.captured_display_id
                && id == captured_id
            {
                return false;
            }
            true
        })
        .map(|&(_, rect)| (rect, true))
        .collect()
}

/// Returns true when at least one Linux display connector looks like a physical output
/// (as opposed to a virtual/headless-only output where blanking could halt capture).
#[allow(dead_code)]
pub(crate) fn linux_has_physical_connector(connectors: &[&str]) -> bool {
    connectors.iter().any(|name| {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }
        let lower = trimmed.to_ascii_lowercase();
        !lower.starts_with("virtual")
            && !lower.starts_with("headless")
            && !lower.starts_with("unknown")
            && !lower.starts_with("meta-")
    })
}

static NEXT_GUARD_ID: AtomicU64 = AtomicU64::new(1);
static PRIVACY_FILTER_GENERATION: AtomicU64 = AtomicU64::new(0);
static EXCLUDED_WINDOW_SETS: Mutex<Vec<(u64, Vec<u32>)>> = Mutex::new(Vec::new());

/// Monotonic generation counter bumped whenever the set of active privacy curtain window IDs
/// changes (used by macOS ScreenCaptureKit to hot-update `SCContentFilter::with_excluding_windows`).
#[allow(dead_code)]
pub fn privacy_filter_generation() -> u64 {
    PRIVACY_FILTER_GENERATION.load(Ordering::Acquire)
}

/// Returns all currently active privacy curtain window IDs (`CGWindowID` on macOS) that screen
/// capture should exclude so remote viewers see the live desktop underneath the black curtain.
#[allow(dead_code)]
pub fn active_privacy_window_ids() -> Vec<u32> {
    let sets = EXCLUDED_WINDOW_SETS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut ids = Vec::new();
    for (_, group) in sets.iter() {
        for &id in group {
            if id != 0 && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

#[allow(dead_code)]
fn register_excluded_window_ids(guard_id: u64, window_ids: Vec<u32>) {
    if window_ids.is_empty() {
        return;
    }
    let mut sets = EXCLUDED_WINDOW_SETS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sets.push((guard_id, window_ids));
    drop(sets);
    PRIVACY_FILTER_GENERATION.fetch_add(1, Ordering::AcqRel);
}

#[allow(dead_code)]
fn unregister_excluded_window_ids(guard_id: u64) {
    let mut sets = EXCLUDED_WINDOW_SETS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = sets.len();
    sets.retain(|(id, _)| *id != guard_id);
    let changed = sets.len() != before;
    drop(sets);
    if changed {
        PRIVACY_FILTER_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

/// RAII guard that blanks/curtains local physical displays while at least one remote viewer is
/// connected and automatically restores them on `Drop`.
pub struct PrivacyScreenGuard {
    guard_id: u64,
    target: PrivacyTarget,
    backend: PlatformPrivacyBackend,
}

impl PrivacyScreenGuard {
    /// Activates the host privacy screen for `target` on a best-effort basis.
    ///
    /// Never fails the remote session if the platform or display configuration does not support
    /// blanking; instead logs a message and returns an inert guard.
    pub fn activate(target: PrivacyTarget) -> Self {
        let guard_id = NEXT_GUARD_ID.fetch_add(1, Ordering::Relaxed);
        let backend = PlatformPrivacyBackend::activate(guard_id, target);
        Self {
            guard_id,
            target,
            backend,
        }
    }

    #[allow(dead_code)]
    pub fn target(&self) -> PrivacyTarget {
        self.target
    }

    #[allow(dead_code)]
    pub fn is_engaged(&self) -> bool {
        self.backend.is_engaged()
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        target: PrivacyTarget,
        window_ids: Vec<u32>,
        on_drop: Box<dyn FnOnce() + Send>,
    ) -> Self {
        let guard_id = NEXT_GUARD_ID.fetch_add(1, Ordering::Relaxed);
        register_excluded_window_ids(guard_id, window_ids);
        Self {
            guard_id,
            target,
            backend: PlatformPrivacyBackend::Test(Some(on_drop)),
        }
    }
}

impl Drop for PrivacyScreenGuard {
    fn drop(&mut self) {
        self.backend.deactivate();
        unregister_excluded_window_ids(self.guard_id);
    }
}

enum PlatformPrivacyBackend {
    #[cfg(target_os = "macos")]
    Macos(macos::MacosPrivacyCurtain),
    #[cfg(target_os = "windows")]
    Windows(windows::WindowsPrivacyCurtain),
    #[cfg(target_os = "linux")]
    Linux(linux::LinuxPrivacyCurtain),
    #[allow(dead_code)]
    Inert,
    #[cfg(test)]
    Test(Option<Box<dyn FnOnce() + Send>>),
}

impl PlatformPrivacyBackend {
    fn activate(guard_id: u64, target: PrivacyTarget) -> Self {
        #[cfg(target_os = "macos")]
        {
            match macos::MacosPrivacyCurtain::activate(guard_id, target) {
                Some(curtain) => Self::Macos(curtain),
                None => Self::Inert,
            }
        }
        #[cfg(target_os = "windows")]
        {
            let _ = guard_id;
            match windows::WindowsPrivacyCurtain::activate(target) {
                Some(curtain) => Self::Windows(curtain),
                None => Self::Inert,
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = guard_id;
            match linux::LinuxPrivacyCurtain::activate(target) {
                Some(curtain) => Self::Linux(curtain),
                None => Self::Inert,
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            let _ = (guard_id, target);
            Self::Inert
        }
    }

    fn is_engaged(&self) -> bool {
        match self {
            Self::Inert => false,
            #[cfg(test)]
            Self::Test(cb) => cb.is_some(),
            #[cfg(target_os = "macos")]
            Self::Macos(_) => true,
            #[cfg(target_os = "windows")]
            Self::Windows(_) => true,
            #[cfg(target_os = "linux")]
            Self::Linux(_) => true,
        }
    }

    fn deactivate(&mut self) {
        match self {
            #[cfg(target_os = "macos")]
            Self::Macos(curtain) => curtain.close(),
            #[cfg(target_os = "windows")]
            Self::Windows(curtain) => curtain.close(),
            #[cfg(target_os = "linux")]
            Self::Linux(curtain) => curtain.restore(),
            Self::Inert => {}
            #[cfg(test)]
            Self::Test(cb) => {
                if let Some(cleanup) = cb.take() {
                    cleanup();
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{ffi::c_void, ptr::NonNull};

    use super::{PrivacyTarget, register_excluded_window_ids};

    const MAX_PRIVACY_WINDOWS: usize = 32;

    unsafe extern "C" {
        fn remoters_macos_privacy_create(
            is_virtual_display: bool,
            captured_display_id: u32,
            out_window_ids: *mut u32,
            max_window_ids: usize,
            out_count: *mut usize,
        ) -> *mut c_void;
        fn remoters_macos_privacy_destroy(handle: *mut c_void);
    }

    pub(super) struct MacosPrivacyCurtain {
        handle: Option<NonNull<c_void>>,
    }

    // SAFETY: `handle` is an opaque retained NSArray<NSWindow*> whose teardown is explicitly
    // dispatched onto the AppKit main queue inside `remoters_macos_privacy_destroy`.
    unsafe impl Send for MacosPrivacyCurtain {}

    impl MacosPrivacyCurtain {
        pub(super) fn activate(guard_id: u64, target: PrivacyTarget) -> Option<Self> {
            let mut window_ids = [0u32; MAX_PRIVACY_WINDOWS];
            let mut count = 0usize;
            let raw = unsafe {
                remoters_macos_privacy_create(
                    target.is_virtual_display,
                    target.captured_display_id.unwrap_or(0),
                    window_ids.as_mut_ptr(),
                    window_ids.len(),
                    &mut count,
                )
            };
            let handle = NonNull::new(raw)?;
            let ids = window_ids[..count.min(window_ids.len())]
                .iter()
                .copied()
                .filter(|&id| id != 0)
                .collect::<Vec<_>>();
            register_excluded_window_ids(guard_id, ids);
            Some(Self {
                handle: Some(handle),
            })
        }

        pub(super) fn close(&mut self) {
            if let Some(handle) = self.handle.take() {
                unsafe { remoters_macos_privacy_destroy(handle.as_ptr()) };
            }
        }
    }

    impl Drop for MacosPrivacyCurtain {
        fn drop(&mut self) {
            self.close();
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::{
        ffi::c_void,
        mem::size_of,
        ptr::{null, null_mut},
        sync::{Once, mpsc},
        thread::{self, JoinHandle},
    };

    use super::{MonitorRect, PrivacyTarget, select_windows_curtain_targets};

    const CS_VREDRAW: u32 = 0x0001;
    const CS_HREDRAW: u32 = 0x0002;
    const BLACK_BRUSH: i32 = 4;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOPMOST: u32 = 0x0000_0008;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x0000_0011;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_SHOWWINDOW: u32 = 0x0040;
    const SW_SHOWNOACTIVATE: i32 = 4;
    const PM_NOREMOVE: u32 = 0x0000;
    const WM_QUIT: u32 = 0x0012;
    const WM_MOUSEACTIVATE: u32 = 0x0021;
    const WM_NCHITTEST: u32 = 0x0084;
    const MA_NOACTIVATE: isize = 3;
    const HTTRANSPARENT: isize = -1;
    const HWND_TOPMOST: *mut c_void = -1isize as *mut c_void;

    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        rc_monitor: MonitorRect,
        rc_work: MonitorRect,
        dw_flags: u32,
    }

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Msg {
        hwnd: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: Point,
    }

    #[repr(C)]
    struct WndClassW {
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut c_void,
        h_icon: *mut c_void,
        h_cursor: *mut c_void,
        hbr_background: *mut c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    type MonitorEnumProc =
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut MonitorRect, isize) -> i32;

    unsafe extern "system" {
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
        fn GetCurrentThreadId() -> u32;
        fn GetStockObject(i: i32) -> *mut c_void;
        fn EnumDisplayMonitors(
            hdc: *mut c_void,
            lprc_clip: *const MonitorRect,
            lpfn_enum: Option<MonitorEnumProc>,
            dw_data: isize,
        ) -> i32;
        fn GetMonitorInfoW(h_monitor: *mut c_void, lpmi: *mut MonitorInfo) -> i32;
        fn RegisterClassW(lp_wnd_class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            h_wnd_parent: *mut c_void,
            h_menu: *mut c_void,
            h_instance: *mut c_void,
            lp_param: *mut c_void,
        ) -> *mut c_void;
        fn SetWindowDisplayAffinity(hwnd: *mut c_void, dw_affinity: u32) -> i32;
        fn SetWindowPos(
            hwnd: *mut c_void,
            hwnd_insert_after: *mut c_void,
            x: i32,
            y: i32,
            cx: i32,
            cy: i32,
            u_flags: u32,
        ) -> i32;
        fn ShowWindow(hwnd: *mut c_void, n_cmd_show: i32) -> i32;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
        fn DefWindowProcW(hwnd: *mut c_void, msg: u32, w_param: usize, l_param: isize) -> isize;
        fn PeekMessageW(
            lp_msg: *mut Msg,
            hwnd: *mut c_void,
            w_msg_filter_min: u32,
            w_msg_filter_max: u32,
            w_remove_msg: u32,
        ) -> i32;
        fn GetMessageW(
            lp_msg: *mut Msg,
            hwnd: *mut c_void,
            w_msg_filter_min: u32,
            w_msg_filter_max: u32,
        ) -> i32;
        fn TranslateMessage(lp_msg: *const Msg) -> i32;
        fn DispatchMessageW(lp_msg: *const Msg) -> isize;
        fn PostThreadMessageW(id_thread: u32, msg: u32, w_param: usize, l_param: isize) -> i32;
    }

    static REGISTER_CLASS: Once = Once::new();

    fn class_name_wide() -> Vec<u16> {
        "RemotersPrivacyCurtainClass\0".encode_utf16().collect()
    }

    unsafe extern "system" fn privacy_wnd_proc(
        hwnd: *mut c_void,
        msg: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        match msg {
            WM_NCHITTEST => HTTRANSPARENT,
            WM_MOUSEACTIVATE => MA_NOACTIVATE,
            _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
        }
    }

    unsafe extern "system" fn enum_monitor_proc(
        h_monitor: *mut c_void,
        _hdc: *mut c_void,
        _lprc: *mut MonitorRect,
        l_param: isize,
    ) -> i32 {
        let mut info = MonitorInfo {
            cb_size: size_of::<MonitorInfo>() as u32,
            rc_monitor: MonitorRect::default(),
            rc_work: MonitorRect::default(),
            dw_flags: 0,
        };
        if unsafe { GetMonitorInfoW(h_monitor, &mut info) } != 0 {
            let monitors = unsafe { &mut *(l_param as *mut Vec<(u32, MonitorRect)>) };
            monitors.push((h_monitor as usize as u32, info.rc_monitor));
        }
        1
    }

    fn enumerate_monitors() -> Vec<(u32, MonitorRect)> {
        let mut monitors = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(enum_monitor_proc),
                (&mut monitors as *mut Vec<(u32, MonitorRect)>) as isize,
            );
        }
        monitors
    }

    pub(super) struct WindowsPrivacyCurtain {
        thread_id: u32,
        worker: Option<JoinHandle<()>>,
    }

    impl WindowsPrivacyCurtain {
        pub(super) fn activate(target: PrivacyTarget) -> Option<Self> {
            let targets = select_windows_curtain_targets(&enumerate_monitors(), target);
            if targets.is_empty() {
                eprintln!(
                    "Host privacy screen: no eligible physical monitor found to blank on Windows."
                );
                return None;
            }

            let (ready_tx, ready_rx) = mpsc::sync_channel::<Option<u32>>(1);
            let worker = thread::Builder::new()
                .name("privacy-screen-win".into())
                .spawn(move || {
                    let class_name = class_name_wide();
                    let h_instance = unsafe { GetModuleHandleW(null()) };
                    REGISTER_CLASS.call_once(|| {
                        let wc = WndClassW {
                            style: CS_HREDRAW | CS_VREDRAW,
                            lpfn_wnd_proc: Some(privacy_wnd_proc),
                            cb_cls_extra: 0,
                            cb_wnd_extra: 0,
                            h_instance,
                            h_icon: null_mut(),
                            h_cursor: null_mut(),
                            hbr_background: unsafe { GetStockObject(BLACK_BRUSH) },
                            lpsz_menu_name: null(),
                            lpsz_class_name: class_name.as_ptr(),
                        };
                        unsafe {
                            RegisterClassW(&wc);
                        }
                    });

                    // Ensure this thread's Win32 message queue exists before publishing `thread_id`.
                    let mut msg = Msg {
                        hwnd: null_mut(),
                        message: 0,
                        w_param: 0,
                        l_param: 0,
                        time: 0,
                        pt: Point { x: 0, y: 0 },
                    };
                    unsafe {
                        PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
                    }

                    let title: Vec<u16> = "Remoters Privacy Screen\0".encode_utf16().collect();
                    let mut hwnds = Vec::with_capacity(targets.len());
                    for (rect, exclude_from_capture) in targets {
                        let hwnd = unsafe {
                            CreateWindowExW(
                                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                                class_name.as_ptr(),
                                title.as_ptr(),
                                WS_POPUP,
                                rect.left,
                                rect.top,
                                rect.width(),
                                rect.height(),
                                null_mut(),
                                null_mut(),
                                h_instance,
                                null_mut(),
                            )
                        };
                        if hwnd.is_null() {
                            continue;
                        }
                        if exclude_from_capture {
                            let ok = unsafe {
                                SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE)
                            };
                            if ok == 0 {
                                eprintln!(
                                    "Host privacy screen: SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) failed; keeping curtain window."
                                );
                            }
                        }
                        unsafe {
                            SetWindowPos(
                                hwnd,
                                HWND_TOPMOST,
                                rect.left,
                                rect.top,
                                rect.width(),
                                rect.height(),
                                SWP_NOACTIVATE | SWP_SHOWWINDOW,
                            );
                            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        }
                        hwnds.push(hwnd);
                    }

                    if hwnds.is_empty() {
                        let _ = ready_tx.send(None);
                        return;
                    }

                    let thread_id = unsafe { GetCurrentThreadId() };
                    let _ = ready_tx.send(Some(thread_id));

                    while unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) } > 0 {
                        unsafe {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }

                    for hwnd in hwnds {
                        unsafe {
                            DestroyWindow(hwnd);
                        }
                    }
                })
                .ok()?;

            match ready_rx.recv().ok().flatten() {
                Some(thread_id) => Some(Self {
                    thread_id,
                    worker: Some(worker),
                }),
                None => {
                    let _ = worker.join();
                    None
                }
            }
        }

        pub(super) fn close(&mut self) {
            if let Some(worker) = self.worker.take() {
                unsafe {
                    PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
                }
                let _ = worker.join();
            }
        }
    }

    impl Drop for WindowsPrivacyCurtain {
        fn drop(&mut self) {
            self.close();
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;

    use super::{PrivacyTarget, linux_has_physical_connector};

    enum LinuxBlankMethod {
        MutterPowerSave,
        X11Dpms,
    }

    pub(super) struct LinuxPrivacyCurtain {
        method: Option<LinuxBlankMethod>,
    }

    impl LinuxPrivacyCurtain {
        pub(super) fn activate(target: PrivacyTarget) -> Option<Self> {
            let connectors: Vec<String> = crate::platform::linux::query_linux_displays()
                .into_iter()
                .map(|d| d.connector)
                .collect();
            let connector_refs: Vec<&str> = connectors.iter().map(String::as_str).collect();
            if !connector_refs.is_empty() && !linux_has_physical_connector(&connector_refs) {
                eprintln!(
                    "Host privacy screen: virtual-only Linux display detected ({:?}); skipping blanking to keep capture alive.",
                    connectors
                );
                return None;
            }

            if crate::platform::wayland_portal::is_wayland_session() {
                if target.is_virtual_display && connector_refs.len() <= 1 {
                    eprintln!(
                        "Host privacy screen: single Wayland virtual mode display; skipping Mutter PowerSaveMode to preserve PipeWire capture."
                    );
                    return None;
                }
                let status = Command::new("busctl")
                    .args([
                        "--user",
                        "set-property",
                        "org.gnome.Mutter.DisplayConfig",
                        "/org/gnome/Mutter/DisplayConfig",
                        "org.gnome.Mutter.DisplayConfig",
                        "PowerSaveMode",
                        "i",
                        "1",
                    ])
                    .status();
                match status {
                    Ok(exit) if exit.success() => {
                        return Some(Self {
                            method: Some(LinuxBlankMethod::MutterPowerSave),
                        });
                    }
                    Ok(exit) => {
                        eprintln!(
                            "Host privacy screen: GNOME Mutter PowerSaveMode unsupported (exit {exit}); continuing without physical blanking."
                        );
                        return None;
                    }
                    Err(err) => {
                        eprintln!(
                            "Host privacy screen: busctl unavailable on Wayland ({err}); continuing without physical blanking."
                        );
                        return None;
                    }
                }
            }

            if std::env::var_os("DISPLAY").is_some() {
                let status = Command::new("xset").args(["dpms", "force", "off"]).status();
                match status {
                    Ok(exit) if exit.success() => {
                        return Some(Self {
                            method: Some(LinuxBlankMethod::X11Dpms),
                        });
                    }
                    Ok(exit) => {
                        eprintln!(
                            "Host privacy screen: xset dpms force off exited with {exit}; continuing without physical blanking."
                        );
                        return None;
                    }
                    Err(err) => {
                        eprintln!(
                            "Host privacy screen: xset unavailable ({err}); continuing without physical blanking."
                        );
                        return None;
                    }
                }
            }

            eprintln!(
                "Host privacy screen: no supported Linux display session found; continuing without physical blanking."
            );
            None
        }

        pub(super) fn restore(&mut self) {
            let Some(method) = self.method.take() else {
                return;
            };
            match method {
                LinuxBlankMethod::MutterPowerSave => {
                    let _ = Command::new("busctl")
                        .args([
                            "--user",
                            "set-property",
                            "org.gnome.Mutter.DisplayConfig",
                            "/org/gnome/Mutter/DisplayConfig",
                            "org.gnome.Mutter.DisplayConfig",
                            "PowerSaveMode",
                            "i",
                            "0",
                        ])
                        .status();
                }
                LinuxBlankMethod::X11Dpms => {
                    let _ = Command::new("xset").args(["dpms", "force", "on"]).status();
                }
            }
        }
    }

    impl Drop for LinuxPrivacyCurtain {
        fn drop(&mut self) {
            self.restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::{
        MonitorRect, PrivacyScreenGuard, PrivacyTarget, active_privacy_window_ids,
        linux_has_physical_connector, privacy_filter_generation, select_windows_curtain_targets,
    };

    #[test]
    fn privacy_guard_raii_registers_window_ids_and_cleans_up_on_drop() {
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleaned_flag = cleaned.clone();
        let gen_before = privacy_filter_generation();
        let target = PrivacyTarget {
            is_virtual_display: false,
            captured_display_id: Some(77),
        };

        let guard = PrivacyScreenGuard::new_for_test(
            target,
            vec![900_001, 900_002],
            Box::new(move || {
                cleaned_flag.store(true, Ordering::SeqCst);
            }),
        );
        assert_eq!(guard.target(), target);
        assert!(guard.is_engaged());
        assert!(privacy_filter_generation() > gen_before);
        let active_ids = active_privacy_window_ids();
        assert!(active_ids.contains(&900_001));
        assert!(active_ids.contains(&900_002));
        assert!(!cleaned.load(Ordering::SeqCst));

        let gen_while_active = privacy_filter_generation();
        drop(guard);

        assert!(cleaned.load(Ordering::SeqCst));
        assert!(privacy_filter_generation() > gen_while_active);
        let after_ids = active_privacy_window_ids();
        assert!(!after_ids.contains(&900_001));
        assert!(!after_ids.contains(&900_002));
    }

    #[test]
    fn windows_monitor_selection_skips_virtual_display_and_curtains_physical_monitors() {
        let primary = MonitorRect {
            left: 0,
            top: 0,
            right: 2560,
            bottom: 1440,
        };
        let secondary = MonitorRect {
            left: 2560,
            top: 0,
            right: 4480,
            bottom: 1080,
        };
        let virtual_mon = MonitorRect {
            left: 4480,
            top: 0,
            right: 6400,
            bottom: 1080,
        };
        let empty = MonitorRect::default();
        let monitors = [
            (10, primary),
            (20, secondary),
            (30, virtual_mon),
            (40, empty),
        ];

        // Virtual display capture: cover physical monitors (10 & 20) only, never the virtual one (30).
        let virtual_targets = select_windows_curtain_targets(
            &monitors,
            PrivacyTarget {
                is_virtual_display: true,
                captured_display_id: Some(30),
            },
        );
        assert_eq!(virtual_targets, vec![(primary, true), (secondary, true)]);

        // Physical display capture: cover all non-empty monitors with WDA_EXCLUDEFROMCAPTURE.
        let physical_targets = select_windows_curtain_targets(
            &monitors,
            PrivacyTarget {
                is_virtual_display: false,
                captured_display_id: Some(10),
            },
        );
        assert_eq!(
            physical_targets,
            vec![(primary, true), (secondary, true), (virtual_mon, true)]
        );
    }

    #[test]
    fn linux_physical_connector_detection_distinguishes_virtual_only_setups() {
        assert!(!linux_has_physical_connector(&[]));
        assert!(!linux_has_physical_connector(&["Virtual-1"]));
        assert!(!linux_has_physical_connector(&["VIRTUAL1", "HEADLESS-1"]));
        assert!(!linux_has_physical_connector(&["Meta-0", "Unknown-1"]));
        assert!(linux_has_physical_connector(&["DP-1"]));
        assert!(linux_has_physical_connector(&["Virtual-1", "HDMI-A-1"]));
        assert!(linux_has_physical_connector(&["eDP-1"]));
    }
}
