use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use remoters_core::{
    session::TransportStats,
    video::{Codec, Format},
};

const REPORT_INTERVAL: Duration = Duration::from_secs(2);
const LIVE_WINDOW: Duration = Duration::from_secs(1);
const WARMUP_WINDOW: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionTelemetry {
    pub width: u16,
    pub height: u16,
    pub target_fps: u16,
    pub codec: &'static str,
    pub hdr: bool,
    pub fps: f64,
    pub video_bps: f64,
    pub net_rx_bytes_per_sec: f64,
    pub net_tx_bytes_per_sec: f64,
    pub rtt_ms: f64,
    pub encode_ms: f64,
    pub decode_ms: f64,
    pub e2e_ms: f64,
    pub lost_packets: u64,
}

struct TelemetryWindow {
    window_start: Instant,
    has_window_sample: bool,
    has_transport_baseline: bool,
    prev_rx_bytes: u64,
    prev_tx_bytes: u64,
    last_transport: TransportStats,
    published: SessionTelemetry,
}

pub struct LiveViewerMetrics {
    present_frames: AtomicU64,
    video_bytes: AtomicU64,
    encode_us_sum: AtomicU64,
    encode_samples: AtomicU64,
    decode_us_sum: AtomicU64,
    decode_samples: AtomicU64,
    e2e_us_sum: AtomicU64,
    e2e_samples: AtomicU64,
    window: Mutex<TelemetryWindow>,
}

impl LiveViewerMetrics {
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    fn new_at(now: Instant) -> Self {
        Self {
            present_frames: AtomicU64::new(0),
            video_bytes: AtomicU64::new(0),
            encode_us_sum: AtomicU64::new(0),
            encode_samples: AtomicU64::new(0),
            decode_us_sum: AtomicU64::new(0),
            decode_samples: AtomicU64::new(0),
            e2e_us_sum: AtomicU64::new(0),
            e2e_samples: AtomicU64::new(0),
            window: Mutex::new(TelemetryWindow {
                window_start: now,
                has_window_sample: false,
                has_transport_baseline: false,
                prev_rx_bytes: 0,
                prev_tx_bytes: 0,
                last_transport: TransportStats::default(),
                published: SessionTelemetry {
                    codec: "—",
                    ..SessionTelemetry::default()
                },
            }),
        }
    }

    pub fn set_format(&self, format: Format) {
        let mut window = self.window.lock().unwrap();
        window.published.width = format.width;
        window.published.height = format.height;
        window.published.target_fps = format.frames_per_second;
        window.published.codec = match format.codec {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        };
        window.published.hdr = format.hdr;
    }

    pub fn record_video_frame(&self, bytes: usize) {
        self.video_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn record_decode(&self, elapsed: Duration) {
        self.decode_us_sum
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.decode_samples.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_e2e(&self, host_us: u32, rtt: Duration, viewer_elapsed: Duration) {
        let host = u64::from(host_us);
        if host > 0 {
            self.encode_us_sum.fetch_add(host, Ordering::Relaxed);
            self.encode_samples.fetch_add(1, Ordering::Relaxed);
        }
        let one_way_us = (rtt.as_micros() as u64) / 2;
        let total_us = host
            .saturating_add(one_way_us)
            .saturating_add(viewer_elapsed.as_micros() as u64);
        self.e2e_us_sum.fetch_add(total_us, Ordering::Relaxed);
        self.e2e_samples.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_present(&self) {
        self.present_frames.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_transport(&self, stats: TransportStats) {
        let mut window = self.window.lock().unwrap();
        if !window.has_transport_baseline {
            window.has_transport_baseline = true;
            window.prev_rx_bytes = stats.rx_bytes;
            window.prev_tx_bytes = stats.tx_bytes;
            window.published.rtt_ms = stats.rtt.as_secs_f64() * 1000.0;
            window.published.lost_packets = stats.lost_packets;
        }
        window.last_transport = stats;
    }

    pub fn snapshot(&self) -> SessionTelemetry {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> SessionTelemetry {
        let mut window = self.window.lock().unwrap();
        let elapsed = now.saturating_duration_since(window.window_start);
        let roll = elapsed >= LIVE_WINDOW;
        let warmup = !window.has_window_sample && elapsed >= WARMUP_WINDOW;
        if (roll || warmup) && !elapsed.is_zero() {
            let (
                frames,
                bytes,
                encode_us,
                encode_samples,
                decode_us,
                decode_samples,
                e2e_us,
                e2e_samples,
            ) = if roll {
                (
                    self.present_frames.swap(0, Ordering::Relaxed),
                    self.video_bytes.swap(0, Ordering::Relaxed),
                    self.encode_us_sum.swap(0, Ordering::Relaxed),
                    self.encode_samples.swap(0, Ordering::Relaxed),
                    self.decode_us_sum.swap(0, Ordering::Relaxed),
                    self.decode_samples.swap(0, Ordering::Relaxed),
                    self.e2e_us_sum.swap(0, Ordering::Relaxed),
                    self.e2e_samples.swap(0, Ordering::Relaxed),
                )
            } else {
                (
                    self.present_frames.load(Ordering::Relaxed),
                    self.video_bytes.load(Ordering::Relaxed),
                    self.encode_us_sum.load(Ordering::Relaxed),
                    self.encode_samples.load(Ordering::Relaxed),
                    self.decode_us_sum.load(Ordering::Relaxed),
                    self.decode_samples.load(Ordering::Relaxed),
                    self.e2e_us_sum.load(Ordering::Relaxed),
                    self.e2e_samples.load(Ordering::Relaxed),
                )
            };

            let secs = elapsed.as_secs_f64();
            window.published.fps = frames as f64 / secs;
            window.published.video_bps = (bytes as f64 * 8.0) / secs;
            if window.has_transport_baseline {
                let rx_delta = window
                    .last_transport
                    .rx_bytes
                    .saturating_sub(window.prev_rx_bytes);
                let tx_delta = window
                    .last_transport
                    .tx_bytes
                    .saturating_sub(window.prev_tx_bytes);
                window.published.net_rx_bytes_per_sec = rx_delta as f64 / secs;
                window.published.net_tx_bytes_per_sec = tx_delta as f64 / secs;
            }
            if encode_samples > 0 {
                window.published.encode_ms = (encode_us as f64 / encode_samples as f64) / 1000.0;
            }
            if decode_samples > 0 {
                window.published.decode_ms = (decode_us as f64 / decode_samples as f64) / 1000.0;
            }
            if e2e_samples > 0 {
                window.published.e2e_ms = (e2e_us as f64 / e2e_samples as f64) / 1000.0;
            }
            window.published.rtt_ms = window.last_transport.rtt.as_secs_f64() * 1000.0;
            window.published.lost_packets = window.last_transport.lost_packets;

            if roll {
                window.has_window_sample = true;
                window.window_start = now;
                if window.has_transport_baseline {
                    window.prev_rx_bytes = window.last_transport.rx_bytes;
                    window.prev_tx_bytes = window.last_transport.tx_bytes;
                }
            }
        }
        window.published
    }
}

pub fn format_bitrate(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "0 kbps".into();
    }
    if bps >= 1_000_000.0 {
        format!("{:.1} Mbps", bps / 1_000_000.0)
    } else if bps >= 1_000.0 {
        format!("{:.0} kbps", bps / 1_000.0)
    } else {
        format!("{bps:.0} bps")
    }
}

pub fn format_byte_rate(bytes_per_sec: f64) -> String {
    if !bytes_per_sec.is_finite() || bytes_per_sec <= 0.0 {
        return "0 B/s".into();
    }
    if bytes_per_sec >= 1_048_576.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_048_576.0)
    } else if bytes_per_sec >= 1_024.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1_024.0)
    } else {
        format!("{bytes_per_sec:.0} B/s")
    }
}

pub fn format_latency(ms: f64) -> String {
    if !ms.is_finite() || ms <= 0.0 {
        return "—".into();
    }
    if ms < 1.0 {
        "< 1 ms".into()
    } else {
        format!("{ms:.1} ms")
    }
}

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

    pub fn record(
        &mut self,
        bytes: usize,
        keyframe: bool,
        sent: bool,
        send_elapsed: Duration,
        rtt: Duration,
    ) {
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use remoters_core::{
        session::TransportStats,
        video::{Codec, Format},
    };

    use super::{LiveViewerMetrics, format_bitrate, format_byte_rate, format_latency};

    #[test]
    fn live_viewer_metrics_computes_window_rates_and_decays_when_idle() {
        let t0 = Instant::now();
        let metrics = LiveViewerMetrics::new_at(t0);
        metrics.set_format(Format {
            codec: Codec::H264,
            width: 3024,
            height: 1900,
            frames_per_second: 60,
            hdr: false,
        });
        metrics.record_transport(TransportStats {
            rx_bytes: 1_000,
            tx_bytes: 200,
            rtt: Duration::from_millis(8),
            lost_packets: 1,
        });

        for _ in 0..30 {
            metrics.record_video_frame(20_000);
            metrics.record_decode(Duration::from_millis(2));
            metrics.record_e2e(1_500, Duration::from_millis(8), Duration::from_millis(3));
            metrics.record_present();
        }
        metrics.record_transport(TransportStats {
            rx_bytes: 601_000,
            tx_bytes: 12_200,
            rtt: Duration::from_millis(12),
            lost_packets: 2,
        });
        // Transport updates wait for the window rollover instead of flickering per sample.
        assert!((metrics.snapshot_at(t0 + Duration::from_millis(50)).rtt_ms - 8.0).abs() < 0.01);

        let warmup = metrics.snapshot_at(t0 + Duration::from_millis(500));
        assert_eq!(warmup.codec, "H.264");
        assert_eq!(warmup.width, 3024);
        assert_eq!(warmup.height, 1900);
        assert!((warmup.fps - 60.0).abs() < 0.01);
        assert!((warmup.video_bps - 9_600_000.0).abs() < 1.0);
        assert!((warmup.net_rx_bytes_per_sec - 1_200_000.0).abs() < 1.0);
        assert!((warmup.net_tx_bytes_per_sec - 24_000.0).abs() < 1.0);
        assert!((warmup.rtt_ms - 12.0).abs() < 0.01);
        assert!((warmup.encode_ms - 1.5).abs() < 0.01);
        assert!((warmup.decode_ms - 2.0).abs() < 0.01);
        // E2E = 1.5ms host + (8ms RTT / 2) + 3ms viewer = 8.5ms
        assert!((warmup.e2e_ms - 8.5).abs() < 0.01);
        assert_eq!(warmup.lost_packets, 2);

        let rolled = metrics.snapshot_at(t0 + Duration::from_secs(1));
        assert!((rolled.fps - 30.0).abs() < 0.01);
        assert!((rolled.video_bps - 4_800_000.0).abs() < 1.0);

        metrics.record_transport(TransportStats {
            rx_bytes: 601_000,
            tx_bytes: 12_200,
            rtt: Duration::from_millis(15),
            lost_packets: 3,
        });
        let idle = metrics.snapshot_at(t0 + Duration::from_secs(2));
        assert_eq!(idle.fps, 0.0);
        assert_eq!(idle.video_bps, 0.0);
        assert_eq!(idle.net_rx_bytes_per_sec, 0.0);
        assert!((idle.rtt_ms - 15.0).abs() < 0.01);
        assert_eq!(idle.lost_packets, 3);
    }

    #[test]
    fn rate_and_latency_formatters_scale_units() {
        assert_eq!(format_bitrate(0.0), "0 kbps");
        assert_eq!(format_bitrate(640_000.0), "640 kbps");
        assert_eq!(format_bitrate(8_400_000.0), "8.4 Mbps");

        assert_eq!(format_byte_rate(0.0), "0 B/s");
        assert_eq!(format_byte_rate(512.0), "512 B/s");
        assert_eq!(format_byte_rate(12_800.0), "12.5 KB/s");
        assert_eq!(format_byte_rate(2_097_152.0), "2.0 MB/s");

        assert_eq!(format_latency(0.0), "—");
        assert_eq!(format_latency(0.4), "< 1 ms");
        assert_eq!(format_latency(8.24), "8.2 ms");
    }
}
