use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};

const REPORT_INTERVAL: Duration = Duration::from_secs(2);

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("REMOTERS_LATENCY_LOG")
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "on"))
    })
}

pub struct HostStats {
    window_start: Instant,
    frames: u64,
    keyframes: u64,
    dropped: u64,
    total_bytes: u64,
    send_us_sum: u64,
    send_us_max: u64,
}

impl HostStats {
    pub fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            keyframes: 0,
            dropped: 0,
            total_bytes: 0,
            send_us_sum: 0,
            send_us_max: 0,
        }
    }

    pub fn record(&mut self, bytes: usize, keyframe: bool, sent: bool, send_elapsed: Duration, rtt: Duration) {
        if !enabled() {
            return;
        }
        let us = send_elapsed.as_micros() as u64;
        if sent {
            self.frames += 1;
            if keyframe {
                self.keyframes += 1;
            }
            self.total_bytes += bytes as u64;
            self.send_us_sum += us;
            self.send_us_max = self.send_us_max.max(us);
        } else {
            self.dropped += 1;
        }
        let elapsed = self.window_start.elapsed();
        if elapsed >= REPORT_INTERVAL {
            let secs = elapsed.as_secs_f64();
            let fps = self.frames as f64 / secs;
            let avg_send_ms = if self.frames > 0 {
                (self.send_us_sum as f64 / self.frames as f64) / 1000.0
            } else {
                0.0
            };
            let max_send_ms = self.send_us_max as f64 / 1000.0;
            let avg_kb = if self.frames > 0 {
                (self.total_bytes as f64 / self.frames as f64) / 1024.0
            } else {
                0.0
            };
            let mbps = (self.total_bytes as f64 * 8.0) / (secs * 1_000_000.0);
            eprintln!(
                "[remoters:host] fps={fps:.1} bitrate={mbps:.1}Mbps avg_frame={avg_kb:.1}KB \
                 send_avg={avg_send_ms:.2}ms send_max={max_send_ms:.2}ms kf={} drop={} rtt={:.2}ms",
                self.keyframes,
                self.dropped,
                rtt.as_secs_f64() * 1000.0,
            );
            *self = Self::new();
        }
    }
}

pub struct EncodeStats {
    window_start: Instant,
    frames: u64,
    encode_us_sum: u64,
    encode_us_max: u64,
    dropped: u64,
}

impl EncodeStats {
    pub fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            encode_us_sum: 0,
            encode_us_max: 0,
            dropped: 0,
        }
    }

    pub fn record(&mut self, encode_us: u64, dropped: bool) {
        if !enabled() {
            return;
        }
        self.frames += 1;
        self.encode_us_sum += encode_us;
        self.encode_us_max = self.encode_us_max.max(encode_us);
        if dropped {
            self.dropped += 1;
        }
        let elapsed = self.window_start.elapsed();
        if elapsed >= REPORT_INTERVAL {
            let fps = self.frames as f64 / elapsed.as_secs_f64();
            let avg_ms = (self.encode_us_sum as f64 / self.frames.max(1) as f64) / 1000.0;
            let max_ms = self.encode_us_max as f64 / 1000.0;
            eprintln!(
                "[remoters:encode] fps={fps:.1} enc_avg={avg_ms:.2}ms enc_max={max_ms:.2}ms queue_drop={}",
                self.dropped,
            );
            *self = Self::new();
        }
    }
}

pub struct ViewerStats {
    window_start: Instant,
    frames: u64,
    buffered_none: u64,
    decode_us_sum: u64,
    decode_us_max: u64,
    ui_wait_us_sum: u64,
    ui_wait_us_max: u64,
    ui_frames: u64,
}

impl ViewerStats {
    pub fn new() -> Self {
        Self {
            window_start: Instant::now(),
            frames: 0,
            buffered_none: 0,
            decode_us_sum: 0,
            decode_us_max: 0,
            ui_wait_us_sum: 0,
            ui_wait_us_max: 0,
            ui_frames: 0,
        }
    }

    pub fn record_decode(&mut self, decode_elapsed: Duration, produced: bool) {
        if !enabled() {
            return;
        }
        let us = decode_elapsed.as_micros() as u64;
        self.frames += 1;
        self.decode_us_sum += us;
        self.decode_us_max = self.decode_us_max.max(us);
        if !produced {
            self.buffered_none += 1;
        }
    }

    pub fn record_present(&mut self, ui_wait: Duration, rtt: Duration) {
        if !enabled() {
            return;
        }
        let us = ui_wait.as_micros() as u64;
        self.ui_frames += 1;
        self.ui_wait_us_sum += us;
        self.ui_wait_us_max = self.ui_wait_us_max.max(us);
        let elapsed = self.window_start.elapsed();
        if elapsed >= REPORT_INTERVAL {
            let fps = self.ui_frames as f64 / elapsed.as_secs_f64();
            let dec_avg = (self.decode_us_sum as f64 / self.frames.max(1) as f64) / 1000.0;
            let dec_max = self.decode_us_max as f64 / 1000.0;
            let ui_avg = (self.ui_wait_us_sum as f64 / self.ui_frames.max(1) as f64) / 1000.0;
            let ui_max = self.ui_wait_us_max as f64 / 1000.0;
            eprintln!(
                "[remoters:viewer] fps={fps:.1} dec_avg={dec_avg:.2}ms dec_max={dec_max:.2}ms \
                 dec_none={} ui_wait_avg={ui_avg:.2}ms ui_wait_max={ui_max:.2}ms rtt={:.2}ms",
                self.buffered_none,
                rtt.as_secs_f64() * 1000.0,
            );
            *self = Self::new();
        }
    }
}
