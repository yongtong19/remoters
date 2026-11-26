//! PipeWire zero-copy triple-buffered capture FFI and decoupled double-buffered
//! latest-frame capture slot for Linux desktop streaming.

use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
#[repr(C)]
pub(super) struct RemotersPwStream {
    _private: [u8; 0],
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    pub(super) fn remoters_pw_start(
        pw_fd: i32,
        node_id: u32,
        out_width: *mut u32,
        out_height: *mut u32,
        out_fourcc: *mut u32,
    ) -> *mut RemotersPwStream;
    fn remoters_pw_acquire_frame(
        s: *mut RemotersPwStream,
        last_seq: *mut u64,
        out_ptr: *mut *const u8,
        out_len: *mut usize,
        out_w: *mut u32,
        out_h: *mut u32,
        out_fourcc: *mut u32,
        out_age_us: *mut u64,
        timeout_ms: u32,
        allow_repeat: i32,
    ) -> i32;
    fn remoters_pw_release_frame(s: *mut RemotersPwStream);
    fn remoters_pw_stop(s: *mut RemotersPwStream);
}

#[cfg(target_os = "linux")]
pub(super) fn fourcc_to_pix_fmt(fourcc: u32) -> &'static str {
    match fourcc {
        1 => "bgr0",
        2 => "bgra",
        3 => "rgb0",
        _ => "rgba",
    }
}

#[cfg(target_os = "linux")]
pub(super) struct PwCaptureHandle(pub(super) *mut RemotersPwStream);

#[cfg(target_os = "linux")]
impl Drop for PwCaptureHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { remoters_pw_stop(self.0) };
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) struct PwFrameGuard<'a> {
    stream: &'a PwCaptureHandle,
    ptr: *const u8,
    len: usize,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) fourcc: u32,
    age_us: u64,
    is_new: bool,
}

#[cfg(target_os = "linux")]
impl<'a> PwFrameGuard<'a> {
    pub(super) fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub(super) fn captured_at(&self) -> Instant {
        if self.is_new {
            Instant::now()
                .checked_sub(Duration::from_micros(self.age_us))
                .unwrap_or_else(Instant::now)
        } else {
            Instant::now()
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PwFrameGuard<'_> {
    fn drop(&mut self) {
        unsafe { remoters_pw_release_frame(self.stream.0) };
    }
}

#[cfg(target_os = "linux")]
pub(super) enum PwAcquireResult<'a> {
    Frame(PwFrameGuard<'a>),
    Timeout,
    Closed,
}

#[cfg(target_os = "linux")]
impl PwCaptureHandle {
    pub(super) fn acquire_frame<'a>(
        &'a self,
        last_seq: &mut u64,
        timeout_ms: u32,
        allow_repeat: bool,
    ) -> PwAcquireResult<'a> {
        let mut ptr: *const u8 = std::ptr::null();
        let mut len = 0usize;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut fourcc = 0u32;
        let mut age_us = 0u64;
        let status = unsafe {
            remoters_pw_acquire_frame(
                self.0,
                last_seq,
                &mut ptr,
                &mut len,
                &mut width,
                &mut height,
                &mut fourcc,
                &mut age_us,
                timeout_ms,
                i32::from(allow_repeat),
            )
        };
        match status {
            1 | 2 if !ptr.is_null() && len > 0 => PwAcquireResult::Frame(PwFrameGuard {
                stream: self,
                ptr,
                len,
                width,
                height,
                fourcc,
                age_us,
                is_new: status == 1,
            }),
            0 => PwAcquireResult::Timeout,
            _ => PwAcquireResult::Closed,
        }
    }
}

/// Decoupled double-buffered latest-frame slot so screen capture never stalls
/// behind encoder submission, and the encoder always consumes the freshest frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct CapturedFrameMeta {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) seq: u64,
    pub(super) captured_at: Instant,
}

struct LatestFrameState {
    buffer: Vec<u8>,
    meta: Option<CapturedFrameMeta>,
    seq: u64,
    closed: bool,
}

pub(super) struct LatestFrameSlot {
    state: Mutex<LatestFrameState>,
    ready: Condvar,
}

impl LatestFrameSlot {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(LatestFrameState {
                buffer: Vec::new(),
                meta: None,
                seq: 0,
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    pub(super) fn publish_swap(
        &self,
        src: &mut Vec<u8>,
        width: u32,
        height: u32,
        captured_at: Instant,
    ) {
        let mut guard = self.state.lock().unwrap();
        std::mem::swap(&mut guard.buffer, src);
        guard.seq = guard.seq.wrapping_add(1);
        guard.meta = Some(CapturedFrameMeta {
            width,
            height,
            seq: guard.seq,
            captured_at,
        });
        self.ready.notify_one();
    }

    pub(super) fn take_latest(
        &self,
        dst: &mut Vec<u8>,
        last_seq: &mut u64,
        timeout: Duration,
    ) -> Option<CapturedFrameMeta> {
        let mut guard = self.state.lock().unwrap();
        if guard.seq == *last_seq && !guard.closed {
            let (next_guard, _) = self
                .ready
                .wait_timeout_while(guard, timeout, |st| st.seq == *last_seq && !st.closed)
                .unwrap();
            guard = next_guard;
        }
        if guard.seq == *last_seq {
            return None;
        }
        let meta = guard.meta?;
        std::mem::swap(&mut guard.buffer, dst);
        *last_seq = meta.seq;
        Some(meta)
    }

    pub(super) fn close(&self) {
        let mut guard = self.state.lock().unwrap();
        guard.closed = true;
        self.ready.notify_all();
    }
}
