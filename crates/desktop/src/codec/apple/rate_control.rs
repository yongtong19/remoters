//! VideoToolbox rate-control helpers (`DataRateLimits` and PTS step clamping).

use super::ffi::RTP_CLOCK;

/// Burst headroom multiplier (`1.2x`) for `kVTCompressionPropertyKey_DataRateLimits`.
const DATA_RATE_LIMIT_BURST_NUM: u64 = 12;
const DATA_RATE_LIMIT_BURST_DEN: u64 = 10;
/// Tight averaging window (`0.25s`) for `kVTCompressionPropertyKey_DataRateLimits` so
/// VideoToolbox cannot emit a multi-megabyte burst frame after static silence.
pub(super) const DATA_RATE_LIMIT_WINDOW_SECS: f64 = 0.25;

/// Computes `(bytes_per_window, window_seconds)` for `kVTCompressionPropertyKey_DataRateLimits`.
pub(super) fn data_rate_limits(bitrate_bps: u32) -> (i64, f64) {
    let bitrate = u64::from(bitrate_bps.max(1));
    let burst_bits_per_sec =
        bitrate.saturating_mul(DATA_RATE_LIMIT_BURST_NUM) / DATA_RATE_LIMIT_BURST_DEN;
    let bytes_per_window = ((burst_bits_per_sec as f64 / 8.0) * DATA_RATE_LIMIT_WINDOW_SECS)
        .round()
        .max(1.0) as i64;
    (bytes_per_window, DATA_RATE_LIMIT_WINDOW_SECS)
}

/// Bounds monotonic presentation timestamp increments sent to VideoToolbox to at most
/// `2 * frame_duration` so a multi-second static screen never builds a large PTS gap
/// that causes rate control to emit a giant burst frame.
#[derive(Debug, Clone)]
pub(super) struct PtsClamper {
    max_step_ticks: u64,
    last_raw_ticks: Option<u64>,
    last_pts_ticks: u64,
}

impl PtsClamper {
    pub(super) fn new(fps: u16) -> Self {
        let frame_ticks = (u64::from(RTP_CLOCK as u32) / u64::from(fps.max(1))).max(1);
        Self {
            max_step_ticks: frame_ticks.saturating_mul(2),
            last_raw_ticks: None,
            last_pts_ticks: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn max_step_ticks(&self) -> u64 {
        self.max_step_ticks
    }

    pub(super) fn next(&mut self, raw_ticks: u64) -> u64 {
        match self.last_raw_ticks {
            None => {
                self.last_raw_ticks = Some(raw_ticks);
                self.last_pts_ticks = raw_ticks;
                raw_ticks
            }
            Some(prev_raw) => {
                let raw_delta = raw_ticks.saturating_sub(prev_raw);
                let step = raw_delta.clamp(1, self.max_step_ticks);
                let next_pts = self.last_pts_ticks.saturating_add(step);
                self.last_raw_ticks = Some(raw_ticks.max(prev_raw));
                self.last_pts_ticks = next_pts;
                next_pts
            }
        }
    }
}
