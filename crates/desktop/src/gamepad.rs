#[cfg(any(target_os = "windows", target_os = "linux"))]
use std::ffi::c_void;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Result;
use remoters_core::GamepadState;

pub const MAX_GAMEPADS: usize = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(8);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RawGamepadState {
    slot: u8,
    connected: u8,
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    left_x: i16,
    left_y: i16,
    right_x: i16,
    right_y: i16,
}

impl From<RawGamepadState> for GamepadState {
    fn from(raw: RawGamepadState) -> Self {
        Self {
            slot: raw.slot.min((MAX_GAMEPADS - 1) as u8),
            connected: raw.connected != 0,
            buttons: raw.buttons,
            left_trigger: raw.left_trigger,
            right_trigger: raw.right_trigger,
            left_x: raw.left_x,
            left_y: raw.left_y,
            right_x: raw.right_x,
            right_y: raw.right_y,
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
impl From<GamepadState> for RawGamepadState {
    fn from(state: GamepadState) -> Self {
        Self {
            slot: state.slot,
            connected: u8::from(state.connected),
            buttons: state.buttons,
            left_trigger: state.left_trigger,
            right_trigger: state.right_trigger,
            left_x: state.left_x,
            left_y: state.left_y,
            right_x: state.right_x,
            right_y: state.right_y,
        }
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn remoters_macos_gamepad_init();
    fn remoters_macos_gamepad_poll(out_states: *mut RawGamepadState);
}

#[cfg(target_os = "windows")]
unsafe extern "C" {
    fn remoters_win_gamepad_poll(out_states: *mut RawGamepadState);
    fn remoters_win_vigem_create() -> *mut c_void;
    fn remoters_win_vigem_apply(ptr: *mut c_void, state: *const RawGamepadState) -> bool;
    fn remoters_win_vigem_reset(ptr: *mut c_void);
    fn remoters_win_vigem_destroy(ptr: *mut c_void);
    fn remoters_win_vigem_last_error(ptr: *mut c_void, buf: *mut u8, cap: usize) -> usize;
    fn remoters_win_vigem_probe(err_buf: *mut u8, err_cap: usize) -> i32;
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn remoters_linux_gamepad_reader_create() -> *mut c_void;
    fn remoters_linux_gamepad_reader_poll(ptr: *mut c_void, out_states: *mut RawGamepadState);
    fn remoters_linux_gamepad_reader_destroy(ptr: *mut c_void);
    fn remoters_linux_uinput_create() -> *mut c_void;
    fn remoters_linux_uinput_apply(ptr: *mut c_void, state: *const RawGamepadState) -> bool;
    fn remoters_linux_uinput_reset(ptr: *mut c_void);
    fn remoters_linux_uinput_destroy(ptr: *mut c_void);
    fn remoters_linux_uinput_last_error(ptr: *mut c_void, buf: *mut u8, cap: usize) -> usize;
}

struct GamepadStateTracker {
    states: [GamepadState; MAX_GAMEPADS],
}

impl GamepadStateTracker {
    fn new() -> Self {
        Self {
            states: std::array::from_fn(|slot| GamepadState {
                slot: slot as u8,
                ..GamepadState::default()
            }),
        }
    }

    fn diff(&self, polled: &[GamepadState; MAX_GAMEPADS]) -> Vec<GamepadState> {
        let mut updates = Vec::new();
        for (slot, next_raw) in polled.iter().enumerate() {
            let next = if next_raw.connected {
                GamepadState {
                    slot: slot as u8,
                    ..*next_raw
                }
            } else {
                GamepadState {
                    slot: slot as u8,
                    ..GamepadState::default()
                }
            };
            if next != self.states[slot] {
                updates.push(next);
            }
        }
        updates
    }

    fn commit(&mut self, state: GamepadState) {
        let slot = usize::from(state.slot);
        if slot < MAX_GAMEPADS {
            self.states[slot] = state;
        }
    }

    fn connected_count(&self) -> u8 {
        self.states.iter().filter(|state| state.connected).count() as u8
    }

    fn disconnect_all(&mut self) -> Vec<GamepadState> {
        let mut updates = Vec::new();
        for (slot, state) in self.states.iter_mut().enumerate() {
            if state.connected {
                let disconnected = GamepadState {
                    slot: slot as u8,
                    ..GamepadState::default()
                };
                *state = disconnected;
                updates.push(disconnected);
            }
        }
        updates
    }
}

struct NativeGamepadPoller {
    #[cfg(target_os = "linux")]
    ptr: *mut c_void,
}

impl NativeGamepadPoller {
    fn new() -> Self {
        #[cfg(target_os = "macos")]
        unsafe {
            remoters_macos_gamepad_init();
        }
        #[cfg(target_os = "linux")]
        let ptr = unsafe { remoters_linux_gamepad_reader_create() };
        Self {
            #[cfg(target_os = "linux")]
            ptr,
        }
    }

    fn poll(&mut self) -> [GamepadState; MAX_GAMEPADS] {
        let mut raw = [RawGamepadState::default(); MAX_GAMEPADS];
        for (slot, item) in raw.iter_mut().enumerate() {
            item.slot = slot as u8;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            remoters_macos_gamepad_poll(raw.as_mut_ptr());
        }
        #[cfg(target_os = "windows")]
        unsafe {
            remoters_win_gamepad_poll(raw.as_mut_ptr());
        }
        #[cfg(target_os = "linux")]
        if !self.ptr.is_null() {
            unsafe {
                remoters_linux_gamepad_reader_poll(self.ptr, raw.as_mut_ptr());
            }
        }
        std::array::from_fn(|i| GamepadState::from(raw[i]))
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativeGamepadPoller {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                remoters_linux_gamepad_reader_destroy(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}

pub struct GamepadReader {
    connected_count: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl GamepadReader {
    pub fn start(
        connected_count: Arc<AtomicU8>,
        on_count: impl Fn(u8) + Send + 'static,
        on_state: impl Fn(GamepadState) -> bool + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_count = connected_count.clone();
        let worker = thread::Builder::new()
            .name("gamepad-reader".into())
            .spawn(move || {
                let mut poller = NativeGamepadPoller::new();
                let mut tracker = GamepadStateTracker::new();
                if worker_count.swap(0, Ordering::AcqRel) != 0 {
                    on_count(0);
                }

                while !worker_stop.load(Ordering::Acquire) {
                    let polled = poller.poll();
                    for update in tracker.diff(&polled) {
                        if on_state(update) {
                            tracker.commit(update);
                        }
                    }
                    let count = tracker.connected_count();
                    if worker_count.swap(count, Ordering::AcqRel) != count {
                        on_count(count);
                    }
                    thread::park_timeout(POLL_INTERVAL);
                }

                for update in tracker.disconnect_all() {
                    let _ = on_state(update);
                }
                if worker_count.swap(0, Ordering::AcqRel) != 0 {
                    on_count(0);
                }
            })?;

        Ok(Self {
            connected_count,
            stop,
            worker: Some(worker),
        })
    }

    #[allow(dead_code)]
    pub fn connected_count(&self) -> Arc<AtomicU8> {
        self.connected_count.clone()
    }
}

impl Drop for GamepadReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

pub struct VirtualGamepadHost {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    ptr: *mut c_void,
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    active_slots: [bool; MAX_GAMEPADS],
    warned: bool,
}

// SAFETY: The native virtual gamepad handle has no thread affinity and is only accessed via `&mut self`.
unsafe impl Send for VirtualGamepadHost {}

impl VirtualGamepadHost {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "windows")]
            ptr: unsafe { remoters_win_vigem_create() },
            #[cfg(target_os = "linux")]
            ptr: unsafe { remoters_linux_uinput_create() },
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            active_slots: [false; MAX_GAMEPADS],
            warned: false,
        }
    }

    pub fn apply(&mut self, state: GamepadState) {
        let slot = usize::from(state.slot);
        if slot >= MAX_GAMEPADS {
            return;
        }

        #[cfg(target_os = "windows")]
        {
            if !state.connected && !self.active_slots[slot] {
                return;
            }
            if self.ptr.is_null() {
                return;
            }
            let raw = RawGamepadState::from(state);
            let ok = unsafe { remoters_win_vigem_apply(self.ptr, &raw) };
            if ok {
                self.active_slots[slot] = state.connected;
            } else if !self.warned {
                self.warned = true;
                let msg = read_native_error(self.ptr, remoters_win_vigem_last_error);
                eprintln!(
                    "Virtual gamepad injection unavailable (install ViGEmBus to enable controller input): {msg}"
                );
            }
        }

        #[cfg(target_os = "linux")]
        {
            if !state.connected && !self.active_slots[slot] {
                return;
            }
            if self.ptr.is_null() {
                return;
            }
            let raw = RawGamepadState::from(state);
            let ok = unsafe { remoters_linux_uinput_apply(self.ptr, &raw) };
            if ok {
                self.active_slots[slot] = state.connected;
            } else if !self.warned {
                self.warned = true;
                let msg = read_native_error(self.ptr, remoters_linux_uinput_last_error);
                eprintln!("Virtual gamepad injection unavailable (/dev/uinput): {msg}");
            }
        }

        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        {
            if state.connected && !self.warned {
                self.warned = true;
                eprintln!(
                    "Virtual gamepad injection is not supported on this host OS; ignoring controller input."
                );
            }
        }
    }

    pub fn reset(&mut self) {
        #[cfg(target_os = "windows")]
        if !self.ptr.is_null() {
            unsafe {
                remoters_win_vigem_reset(self.ptr);
            }
            self.active_slots = [false; MAX_GAMEPADS];
        }

        #[cfg(target_os = "linux")]
        if !self.ptr.is_null() {
            unsafe {
                remoters_linux_uinput_reset(self.ptr);
            }
            self.active_slots = [false; MAX_GAMEPADS];
        }
    }

    #[cfg(target_os = "windows")]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn probe_vigembus() -> std::result::Result<bool, String> {
        let mut buf = [0u8; 256];
        let status = unsafe { remoters_win_vigem_probe(buf.as_mut_ptr(), buf.len()) };
        match status {
            0 => Ok(true),
            1 => Ok(false),
            _ => {
                let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                Err(String::from_utf8_lossy(&buf[..len]).into_owned())
            }
        }
    }
}

impl Default for VirtualGamepadHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VirtualGamepadHost {
    fn drop(&mut self) {
        self.reset();
        #[cfg(target_os = "windows")]
        if !self.ptr.is_null() {
            unsafe {
                remoters_win_vigem_destroy(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
        #[cfg(target_os = "linux")]
        if !self.ptr.is_null() {
            unsafe {
                remoters_linux_uinput_destroy(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn read_native_error(
    ptr: *mut c_void,
    getter: unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> usize,
) -> String {
    let mut buf = [0u8; 256];
    let len = unsafe { getter(ptr, buf.as_mut_ptr(), buf.len()) }.min(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_emits_only_changes_and_resets_on_disconnect() {
        let mut tracker = GamepadStateTracker::new();
        let mut polled = std::array::from_fn(|slot| GamepadState {
            slot: slot as u8,
            ..GamepadState::default()
        });

        assert!(tracker.diff(&polled).is_empty());
        assert_eq!(tracker.connected_count(), 0);

        polled[0] = GamepadState {
            slot: 0,
            connected: true,
            buttons: GamepadState::A,
            left_trigger: 120,
            right_trigger: 0,
            left_x: 10_000,
            left_y: 20_000,
            right_x: -5_000,
            right_y: 0,
        };
        let updates = tracker.diff(&polled);
        assert_eq!(updates, vec![polled[0]]);
        tracker.commit(updates[0]);
        assert_eq!(tracker.connected_count(), 1);

        // No change on second poll.
        assert!(tracker.diff(&polled).is_empty());

        // Second controller connects.
        polled[2] = GamepadState {
            slot: 2,
            connected: true,
            buttons: GamepadState::START,
            ..GamepadState::default()
        };
        let updates = tracker.diff(&polled);
        assert_eq!(updates, vec![polled[2]]);
        tracker.commit(updates[0]);
        assert_eq!(tracker.connected_count(), 2);

        // Disconnecting slot 0 zeros its fields even if stale values remained in polled.
        polled[0].connected = false;
        let updates = tracker.diff(&polled);
        assert_eq!(
            updates,
            vec![GamepadState {
                slot: 0,
                connected: false,
                ..GamepadState::default()
            }]
        );
        tracker.commit(updates[0]);
        assert_eq!(tracker.connected_count(), 1);

        // Shutdown disconnects remaining active slots.
        let shutdown = tracker.disconnect_all();
        assert_eq!(
            shutdown,
            vec![GamepadState {
                slot: 2,
                connected: false,
                ..GamepadState::default()
            }]
        );
        assert_eq!(tracker.connected_count(), 0);
    }

    #[test]
    fn tracker_defers_commit_when_send_is_rejected() {
        let tracker = GamepadStateTracker::new();
        let mut polled = std::array::from_fn(|slot| GamepadState {
            slot: slot as u8,
            ..GamepadState::default()
        });
        polled[1] = GamepadState {
            slot: 1,
            connected: true,
            buttons: GamepadState::X,
            ..GamepadState::default()
        };

        let first = tracker.diff(&polled);
        assert_eq!(first.len(), 1);
        // Simulate channel full: do not commit.
        let retry = tracker.diff(&polled);
        assert_eq!(retry, first);
    }

    #[test]
    fn gamepad_reader_starts_and_stops_cleanly() {
        let count = Arc::new(AtomicU8::new(0));
        let reader =
            GamepadReader::start(count.clone(), |_| {}, |_| true).expect("start gamepad reader");
        std::thread::sleep(Duration::from_millis(20));
        drop(reader);
        assert_eq!(count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn raw_gamepad_state_converts_and_clamps_slot() {
        let raw = RawGamepadState {
            slot: 7,
            connected: 1,
            buttons: GamepadState::B | GamepadState::GUIDE,
            left_trigger: 200,
            right_trigger: 55,
            left_x: -32768,
            left_y: 32767,
            right_x: 1234,
            right_y: -4321,
        };
        let converted = GamepadState::from(raw);
        assert_eq!(converted.slot, 3);
        assert!(converted.connected);
        assert_eq!(converted.buttons, GamepadState::B | GamepadState::GUIDE);
        assert_eq!(converted.left_trigger, 200);
        assert_eq!(converted.right_trigger, 55);
        assert_eq!(converted.left_x, -32768);
        assert_eq!(converted.left_y, 32767);
        assert_eq!(converted.right_x, 1234);
        assert_eq!(converted.right_y, -4321);
    }

    #[test]
    fn virtual_gamepad_host_degrades_gracefully_without_panicking() {
        let mut host = VirtualGamepadHost::new();
        host.apply(GamepadState {
            slot: 0,
            connected: true,
            buttons: GamepadState::A,
            left_y: 16384,
            ..GamepadState::default()
        });
        host.apply(GamepadState {
            slot: 0,
            connected: false,
            ..GamepadState::default()
        });
        host.reset();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn vigembus_probe_runs_cleanly() {
        match VirtualGamepadHost::probe_vigembus() {
            Ok(true) => eprintln!("ViGEmBus probe: driver installed and XUSB roundtrip succeeded."),
            Ok(false) => eprintln!("ViGEmBus probe: driver not installed on this machine."),
            Err(err) => panic!("ViGEmBus probe failed unexpectedly: {err}"),
        }
    }
}
