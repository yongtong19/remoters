#[cfg(test)]
use std::collections::VecDeque;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::ffi::c_void;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::thread::{self, JoinHandle};
#[cfg(target_os = "linux")]
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::Mutex,
};
#[cfg(target_os = "macos")]
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

use remoters_core::session::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioFrame};

pub const SAMPLE_RATE: u32 = AUDIO_SAMPLE_RATE;
pub const CHANNELS: u8 = AUDIO_CHANNELS;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub const CHUNK_FRAMES: usize = 960;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub const CHUNK_SAMPLES: usize = CHUNK_FRAMES * CHANNELS as usize;
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub const SILENCE_TAIL_CHUNKS: usize = 5;
#[cfg(test)]
pub const TARGET_DRIFT_SAMPLES: usize = 3_840;
#[cfg(test)]
pub const MAX_DRIFT_SAMPLES: usize = 9_600;
#[cfg(test)]
pub const RING_CAPACITY_SAMPLES: usize = 24_000;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn remoters_macos_audio_player_create() -> *mut c_void;
    fn remoters_macos_audio_player_push(player: *mut c_void, samples: *const i16, count: usize);
    fn remoters_macos_audio_player_destroy(player: *mut c_void);
}

#[cfg(target_os = "windows")]
unsafe extern "C" {
    fn remoters_win_audio_player_create() -> *mut c_void;
    fn remoters_win_audio_player_push(player: *mut c_void, samples: *const i16, count: usize);
    fn remoters_win_audio_player_destroy(player: *mut c_void);
    fn remoters_win_audio_capture_start(
        callback: unsafe extern "C" fn(*mut c_void, *const i16, usize),
        ctx: *mut c_void,
    ) -> *mut c_void;
    fn remoters_win_audio_capture_stop(capture: *mut c_void);
}

#[cfg(test)]
pub fn float32_to_i16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    if sample >= 1.0 {
        return i16::MAX;
    }
    if sample <= -1.0 {
        return i16::MIN;
    }
    (sample * 32767.0)
        .round()
        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
}

#[cfg(test)]
pub fn planar_f32_to_stereo_i16(channels: &[&[f32]]) -> Vec<i16> {
    if channels.is_empty() {
        return Vec::new();
    }
    let frames = channels.iter().map(|ch| ch.len()).min().unwrap_or(0);
    if frames == 0 {
        return Vec::new();
    }
    let left = channels[0];
    let right = if channels.len() >= 2 {
        channels[1]
    } else {
        channels[0]
    };
    let mut out = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        out.push(float32_to_i16(left[i]));
        out.push(float32_to_i16(right[i]));
    }
    out
}

#[cfg(test)]
pub fn interleaved_f32_to_stereo_i16(samples: &[f32], channel_count: usize) -> Vec<i16> {
    if channel_count == 0 || samples.len() < channel_count {
        return Vec::new();
    }
    let frames = samples.len() / channel_count;
    let right_ch = usize::from(channel_count >= 2);
    let mut out = Vec::with_capacity(frames * 2);
    for frame in samples.chunks_exact(channel_count) {
        out.push(float32_to_i16(frame[0]));
        out.push(float32_to_i16(frame[right_ch]));
    }
    out
}

pub fn mono_to_stereo_i16(samples: &[i16]) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        out.push(sample);
        out.push(sample);
    }
    out
}

pub fn resample_stereo_i16(samples: &[i16], in_rate: u32, out_rate: u32) -> Vec<i16> {
    let in_frames = samples.len() / 2;
    if in_frames == 0 || in_rate == 0 || out_rate == 0 {
        return Vec::new();
    }
    if in_rate == out_rate {
        return samples[..in_frames * 2].to_vec();
    }
    let out_frames =
        ((in_frames as f64 * f64::from(out_rate) / f64::from(in_rate)).round() as usize).max(1);
    let ratio = f64::from(in_rate) / f64::from(out_rate);
    let mut out = Vec::with_capacity(out_frames * 2);
    for f in 0..out_frames {
        let pos = f as f64 * ratio;
        let idx0 = (pos as usize).min(in_frames - 1);
        let idx1 = (idx0 + 1).min(in_frames - 1);
        let frac = pos - idx0 as f64;
        let l0 = f64::from(samples[idx0 * 2]);
        let l1 = f64::from(samples[idx1 * 2]);
        let r0 = f64::from(samples[idx0 * 2 + 1]);
        let r1 = f64::from(samples[idx1 * 2 + 1]);
        let l = (l0 + (l1 - l0) * frac)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        let r = (r0 + (r1 - r0) * frac)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        out.push(l);
        out.push(r);
    }
    out
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub fn is_silent_pcm(samples: &[i16]) -> bool {
    samples.iter().all(|&sample| sample == 0)
}

#[cfg(any(target_os = "linux", test))]
pub fn pcm_i16_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(any(target_os = "linux", test))]
pub fn le_bytes_to_pcm_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub struct AudioChunker {
    pending: Vec<i16>,
    silent_tail: usize,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl Default for AudioChunker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl AudioChunker {
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(CHUNK_SAMPLES * 2),
            silent_tail: 0,
        }
    }

    pub fn push(&mut self, samples: &[i16], mut on_chunk: impl FnMut(Vec<i16>)) {
        if samples.is_empty() {
            return;
        }
        let aligned = samples.len() & !1;
        if aligned == 0 {
            return;
        }
        self.pending.extend_from_slice(&samples[..aligned]);

        let mut offset = 0;
        while self.pending.len() - offset >= CHUNK_SAMPLES {
            let chunk = &self.pending[offset..offset + CHUNK_SAMPLES];
            if is_silent_pcm(chunk) {
                if self.silent_tail < SILENCE_TAIL_CHUNKS {
                    self.silent_tail += 1;
                    on_chunk(chunk.to_vec());
                }
            } else {
                self.silent_tail = 0;
                on_chunk(chunk.to_vec());
            }
            offset += CHUNK_SAMPLES;
        }
        if offset > 0 {
            self.pending.drain(..offset);
        }
    }
}

#[cfg(test)]
pub struct AudioRingBuffer {
    buf: VecDeque<i16>,
    capacity: usize,
    target_samples: usize,
    max_samples: usize,
}

#[cfg(test)]
impl Default for AudioRingBuffer {
    fn default() -> Self {
        Self::new(
            RING_CAPACITY_SAMPLES,
            TARGET_DRIFT_SAMPLES,
            MAX_DRIFT_SAMPLES,
        )
    }
}

#[cfg(test)]
impl AudioRingBuffer {
    pub fn new(capacity: usize, target_samples: usize, max_samples: usize) -> Self {
        let capacity = (capacity & !1).max(2);
        let max_samples = (max_samples & !1).min(capacity).max(2);
        let target_samples = (target_samples & !1).min(max_samples);
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            target_samples,
            max_samples,
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn push(&mut self, mut samples: &[i16]) {
        let aligned = samples.len() & !1;
        if aligned == 0 {
            return;
        }
        samples = &samples[..aligned];

        if samples.len() >= self.max_samples {
            let keep = self.target_samples & !1;
            samples = &samples[samples.len() - keep..];
            self.buf.clear();
        } else if self.buf.len() + samples.len() > self.max_samples {
            let desired_existing = self.target_samples.saturating_sub(samples.len()) & !1;
            if self.buf.len() > desired_existing {
                let drop_count = (self.buf.len() - desired_existing) & !1;
                self.buf.drain(..drop_count);
            }
        }

        if self.buf.len() + samples.len() > self.capacity {
            let overflow = (self.buf.len() + samples.len() - self.capacity + 1) & !1;
            let drop_count = overflow.min(self.buf.len());
            self.buf.drain(..drop_count);
        }

        self.buf.extend(samples);
    }

    pub fn pop_into(&mut self, dst: &mut [i16]) -> usize {
        let copied = self.buf.len().min(dst.len());
        for slot in dst.iter_mut().take(copied) {
            *slot = self.buf.pop_front().unwrap_or(0);
        }
        if copied < dst.len() {
            dst[copied..].fill(0);
        }
        copied
    }
}

#[cfg(target_os = "linux")]
fn spawn_linux_playback_process() -> Option<Child> {
    let pw = Command::new("pw-cat")
        .args([
            "--playback",
            "--rate",
            "48000",
            "--channels",
            "2",
            "--format",
            "s16",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(child) = pw {
        return Some(child);
    }
    Command::new("pacat")
        .args([
            "--playback",
            "--rate=48000",
            "--channels=2",
            "--format=s16le",
            "--latency-msec=20",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(target_os = "linux")]
fn start_linux_player() -> Option<(std::sync::mpsc::SyncSender<Vec<i16>>, JoinHandle<()>)> {
    let mut child = spawn_linux_playback_process()?;
    let mut stdin = child.stdin.take()?;
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(8);
    let worker = thread::Builder::new()
        .name("linux-audio-player".into())
        .spawn(move || {
            while let Ok(samples) = rx.recv() {
                let bytes = pcm_i16_to_le_bytes(&samples);
                if stdin.write_all(&bytes).is_err() || stdin.flush().is_err() {
                    break;
                }
            }
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
        })
        .ok()?;
    Some((tx, worker))
}

#[cfg(target_os = "linux")]
fn linux_monitor_source() -> String {
    if let Ok(output) = Command::new("pactl").arg("get-default-sink").output()
        && output.status.success()
    {
        let sink = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !sink.is_empty() {
            return format!("{sink}.monitor");
        }
    }
    "@DEFAULT_MONITOR@".to_owned()
}

pub struct AudioPlayer {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    handle: *mut c_void,
    #[cfg(target_os = "linux")]
    sender: Option<std::sync::mpsc::SyncSender<Vec<i16>>>,
    #[cfg(target_os = "linux")]
    worker: Option<JoinHandle<()>>,
    muted: Arc<AtomicBool>,
}

// SAFETY: The native audio player handles (`RemotersMacosAudioPlayer` and `PlayerState`)
// protect all mutable state internally with mutexes and join/dispose on Drop.
unsafe impl Send for AudioPlayer {}
unsafe impl Sync for AudioPlayer {}

impl AudioPlayer {
    pub fn new(muted: Arc<AtomicBool>) -> Self {
        #[cfg(target_os = "macos")]
        let handle = unsafe { remoters_macos_audio_player_create() };
        #[cfg(target_os = "windows")]
        let handle = unsafe { remoters_win_audio_player_create() };
        #[cfg(target_os = "linux")]
        let (sender, worker) = match start_linux_player() {
            Some((tx, handle)) => (Some(tx), Some(handle)),
            None => (None, None),
        };

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if handle.is_null() {
            eprintln!("System audio playback unavailable; continuing without audio output.");
        }
        #[cfg(target_os = "linux")]
        if sender.is_none() {
            eprintln!(
                "System audio playback unavailable (pw-cat/pacat missing); continuing without audio output."
            );
        }

        Self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            handle,
            #[cfg(target_os = "linux")]
            sender,
            #[cfg(target_os = "linux")]
            worker,
            muted,
        }
    }

    fn is_available(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            !self.handle.is_null()
        }
        #[cfg(target_os = "linux")]
        {
            self.sender.is_some()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            false
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Release);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Acquire)
    }

    pub fn push_frame(&self, frame: &AudioFrame) {
        self.push_pcm(&frame.pcm, frame.sample_rate, frame.channels);
    }

    pub fn push_pcm(&self, pcm: &[i16], sample_rate: u32, channels: u8) {
        if !self.is_available() || self.is_muted() || pcm.is_empty() {
            return;
        }
        if sample_rate == SAMPLE_RATE && channels == CHANNELS {
            self.push_stereo_48k(pcm);
            return;
        }
        let stereo = if channels == 1 {
            mono_to_stereo_i16(pcm)
        } else {
            pcm.to_vec()
        };
        let resampled = resample_stereo_i16(&stereo, sample_rate, SAMPLE_RATE);
        if !resampled.is_empty() {
            self.push_stereo_48k(&resampled);
        }
    }

    fn push_stereo_48k(&self, pcm: &[i16]) {
        if !self.is_available() || pcm.len() < 2 {
            return;
        }
        #[cfg(target_os = "macos")]
        unsafe {
            remoters_macos_audio_player_push(self.handle, pcm.as_ptr(), pcm.len());
        }
        #[cfg(target_os = "windows")]
        unsafe {
            remoters_win_audio_player_push(self.handle, pcm.as_ptr(), pcm.len());
        }
        #[cfg(target_os = "linux")]
        if let Some(sender) = &self.sender {
            let aligned = pcm.len() & !1;
            if aligned > 0 {
                let _ = sender.try_send(pcm[..aligned].to_vec());
            }
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if self.handle.is_null() {
                return;
            }
            #[cfg(target_os = "macos")]
            unsafe {
                remoters_macos_audio_player_destroy(self.handle);
            }
            #[cfg(target_os = "windows")]
            unsafe {
                remoters_win_audio_player_destroy(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
        #[cfg(target_os = "linux")]
        {
            drop(self.sender.take());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

pub type AudioSink = Arc<dyn Fn(AudioFrame) + Send + Sync>;

#[cfg(target_os = "windows")]
struct WinCaptureCtx {
    sink: AudioSink,
}

#[cfg(target_os = "windows")]
unsafe extern "C" fn on_win_audio_chunk(
    ctx: *mut c_void,
    samples: *const i16,
    sample_count: usize,
) {
    if ctx.is_null() || samples.is_null() || sample_count == 0 {
        return;
    }
    let state = unsafe { &*ctx.cast::<WinCaptureCtx>() };
    let pcm = unsafe { std::slice::from_raw_parts(samples, sample_count) };
    if let Ok(frame) = AudioFrame::new(SAMPLE_RATE, CHANNELS, pcm.to_vec()) {
        (state.sink)(frame);
    }
}

pub struct HostAudioCapture {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    stop: Arc<AtomicBool>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    worker: Option<JoinHandle<()>>,
    #[cfg(target_os = "linux")]
    child: Arc<Mutex<Option<Child>>>,
    #[cfg(target_os = "windows")]
    handle: *mut c_void,
    #[cfg(target_os = "windows")]
    ctx: *mut c_void,
}

#[cfg(target_os = "windows")]
unsafe impl Send for HostAudioCapture {}

impl HostAudioCapture {
    #[cfg(target_os = "macos")]
    pub fn start_macos(sink: AudioSink, rx: Receiver<Vec<i16>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = match thread::Builder::new()
            .name("host-audio".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(pcm) => {
                            if let Ok(frame) = AudioFrame::new(SAMPLE_RATE, CHANNELS, pcm) {
                                sink(frame);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            }) {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("Host audio thread unavailable; continuing video only: {error}");
                None
            }
        };
        Self { stop, worker }
    }

    #[cfg(target_os = "windows")]
    pub fn start_windows(sink: AudioSink) -> Self {
        let ctx_box = Box::new(WinCaptureCtx { sink });
        let ctx = Box::into_raw(ctx_box).cast::<c_void>();
        let handle = unsafe { remoters_win_audio_capture_start(on_win_audio_chunk, ctx) };
        if handle.is_null() {
            eprintln!("Windows loopback audio capture unavailable; continuing video only.");
            unsafe { drop(Box::from_raw(ctx.cast::<WinCaptureCtx>())) };
            return Self {
                handle: std::ptr::null_mut(),
                ctx: std::ptr::null_mut(),
            };
        }
        Self { handle, ctx }
    }

    #[cfg(target_os = "linux")]
    pub fn start_linux(sink: AudioSink) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let child_slot = Arc::new(Mutex::new(None::<Child>));
        let source = linux_monitor_source();
        let mut command = Command::new("parec");
        command
            .args([
                &format!("--device={source}"),
                "--rate=48000",
                "--channels=2",
                "--format=s16le",
                "--latency-msec=20",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let Ok(mut child) = command.spawn() else {
            eprintln!(
                "Linux loopback audio capture unavailable (parec missing); continuing video only."
            );
            return Self {
                stop,
                worker: None,
                child: child_slot,
            };
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Self {
                stop,
                worker: None,
                child: child_slot,
            };
        };
        *child_slot.lock().unwrap() = Some(child);
        let worker_stop = stop.clone();
        let worker_child = child_slot.clone();
        let worker = match thread::Builder::new()
            .name("host-audio".into())
            .spawn(move || {
                let mut chunker = AudioChunker::new();
                let mut buf = [0u8; CHUNK_SAMPLES * 2];
                while !worker_stop.load(Ordering::Acquire) {
                    match stdout.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let aligned = n & !1;
                            if aligned == 0 {
                                continue;
                            }
                            let pcm = le_bytes_to_pcm_i16(&buf[..aligned]);
                            chunker.push(&pcm, |chunk| {
                                if let Ok(frame) = AudioFrame::new(SAMPLE_RATE, CHANNELS, chunk) {
                                    sink(frame);
                                }
                            });
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                if let Some(mut proc_child) = worker_child.lock().unwrap().take() {
                    let _ = proc_child.kill();
                    let _ = proc_child.wait();
                }
            }) {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("Host audio thread unavailable; continuing video only: {error}");
                if let Some(mut proc_child) = child_slot.lock().unwrap().take() {
                    let _ = proc_child.kill();
                    let _ = proc_child.wait();
                }
                None
            }
        };
        Self {
            stop,
            worker,
            child: child_slot,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    pub fn start_noop(_sink: AudioSink) -> Self {
        let _ = _sink;
        Self {}
    }
}

impl Drop for HostAudioCapture {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
        #[cfg(target_os = "linux")]
        {
            self.stop.store(true, Ordering::Release);
            if let Some(mut proc_child) = self.child.lock().unwrap().take() {
                let _ = proc_child.kill();
                let _ = proc_child.wait();
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
        #[cfg(target_os = "windows")]
        {
            if !self.handle.is_null() {
                unsafe { remoters_win_audio_capture_stop(self.handle) };
                self.handle = std::ptr::null_mut();
            }
            if !self.ctx.is_null() {
                unsafe { drop(Box::from_raw(self.ctx.cast::<WinCaptureCtx>())) };
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use remoters_core::session::AudioFrame;

    use super::{
        AudioChunker, AudioPlayer, AudioRingBuffer, CHANNELS, CHUNK_SAMPLES, MAX_DRIFT_SAMPLES,
        SAMPLE_RATE, SILENCE_TAIL_CHUNKS, TARGET_DRIFT_SAMPLES, float32_to_i16,
        interleaved_f32_to_stereo_i16, is_silent_pcm, le_bytes_to_pcm_i16, pcm_i16_to_le_bytes,
        planar_f32_to_stereo_i16, resample_stereo_i16,
    };

    #[test]
    fn converts_float32_to_i16_with_clamping_and_nan_safety() {
        assert_eq!(float32_to_i16(0.0), 0);
        assert_eq!(float32_to_i16(1.0), i16::MAX);
        assert_eq!(float32_to_i16(1.75), i16::MAX);
        assert_eq!(float32_to_i16(-1.0), i16::MIN);
        assert_eq!(float32_to_i16(-2.5), i16::MIN);
        assert_eq!(float32_to_i16(f32::NAN), 0);
        assert!((float32_to_i16(0.5) - 16384).abs() <= 1);
    }

    #[test]
    fn converts_planar_and_interleaved_f32_to_stereo_i16() {
        let left = [0.5_f32, -0.5];
        let right = [-0.25_f32, 0.25];
        let planar = planar_f32_to_stereo_i16(&[&left, &right]);
        assert_eq!(planar.len(), 4);
        assert!((planar[0] - 16384).abs() <= 1);
        assert!((planar[1] + 8192).abs() <= 1);
        assert!((planar[2] + 16384).abs() <= 1);
        assert!((planar[3] - 8192).abs() <= 1);

        let mono = planar_f32_to_stereo_i16(&[&[0.5_f32]]);
        assert_eq!(mono[0], mono[1]);

        let interleaved = interleaved_f32_to_stereo_i16(&[0.5, -0.25, -0.5, 0.25], 2);
        assert_eq!(interleaved, planar);
    }

    #[test]
    fn resamples_stereo_i16_proportionally() {
        let input = [1000_i16, -1000].repeat(441); // 10 ms at 44.1 kHz
        let output = resample_stereo_i16(&input, 44_100, 48_000);
        assert_eq!(output.len(), 480 * 2);
        assert_eq!(output[0], 1000);
        assert_eq!(output[1], -1000);

        let passthrough = resample_stereo_i16(&input, 48_000, 48_000);
        assert_eq!(passthrough, input);
    }

    #[test]
    fn chunker_skips_silence_after_short_tail() {
        let mut chunker = AudioChunker::new();
        let mut emitted = Vec::new();

        // Active chunk followed by 8 silent chunks -> active + 5 tail chunks emitted, 3 skipped.
        let mut samples = vec![42_i16; CHUNK_SAMPLES];
        samples.extend(vec![0_i16; CHUNK_SAMPLES * (SILENCE_TAIL_CHUNKS + 3)]);
        chunker.push(&samples, |chunk| emitted.push(chunk));
        assert_eq!(emitted.len(), 1 + SILENCE_TAIL_CHUNKS);
        assert!(!is_silent_pcm(&emitted[0]));
        assert!(is_silent_pcm(&emitted[1]));

        // Non-zero audio resets silence tail counter.
        let active = vec![7_i16; CHUNK_SAMPLES];
        chunker.push(&active, |chunk| emitted.push(chunk));
        assert_eq!(emitted.len(), 2 + SILENCE_TAIL_CHUNKS);
    }

    #[test]
    fn ring_buffer_trims_oldest_excess_drift_and_zero_fills_underrun() {
        let mut ring = AudioRingBuffer::default();
        let chunk = vec![11_i16; 1_920]; // 20 ms
        for _ in 0..5 {
            ring.push(&chunk);
        }
        assert_eq!(ring.len(), MAX_DRIFT_SAMPLES);

        // Pushing another 20 ms exceeds 100 ms max drift -> trims down to 40 ms target.
        let latest = vec![99_i16; 1_920];
        ring.push(&latest);
        assert_eq!(ring.len(), TARGET_DRIFT_SAMPLES);

        let mut out = vec![-1_i16; TARGET_DRIFT_SAMPLES + 10];
        let copied = ring.pop_into(&mut out);
        assert_eq!(copied, TARGET_DRIFT_SAMPLES);
        assert_eq!(
            &out[TARGET_DRIFT_SAMPLES - 1_920..TARGET_DRIFT_SAMPLES],
            &latest[..]
        );
        assert!(out[TARGET_DRIFT_SAMPLES..].iter().all(|&s| s == 0));
    }

    #[test]
    fn player_toggles_mute_and_accepts_frames_gracefully() {
        let muted = Arc::new(AtomicBool::new(false));
        let player = AudioPlayer::new(muted);
        assert!(!player.is_muted());
        player.set_muted(true);
        assert!(player.is_muted());
        let frame = AudioFrame::new(SAMPLE_RATE, CHANNELS, vec![0_i16; CHUNK_SAMPLES]).unwrap();
        player.push_frame(&frame);
        player.set_muted(false);
        assert!(!player.is_muted());
    }

    #[test]
    fn round_trips_pcm_i16_little_endian_bytes() {
        let samples = [0_i16, -32768, 32767, -1, 1234, -5678];
        let bytes = pcm_i16_to_le_bytes(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
        assert_eq!(le_bytes_to_pcm_i16(&bytes), samples);
        assert_eq!(le_bytes_to_pcm_i16(&[1]), Vec::<i16>::new());
    }
}
