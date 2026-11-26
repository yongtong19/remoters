//! Low-latency congestion controller tailored for bursty real-time video streaming.
//!
//! Quinn's built-in BBR implementation enters `ProbeRtt` after 10 seconds without a new
//! `min_rtt`, clamping `cwnd` down to `4 * MTU` (~4.8 KiB) for 200 ms. In remote desktop
//! sessions, a static desktop produces almost no traffic until a sudden scene change or
//! scroll emits a large burst frame—often coinciding with `ProbeRtt` or idle cwnd decay.
//!
//! [`RealtimeVideoConfig`] avoids those stalls by:
//! - Starting with a 512 KiB congestion window so large keyframe/scene-change bursts ship
//!   in a single flight.
//! - Enforcing a hard 256 KiB floor so isolated Wi-Fi packet loss or burst silence never
//!   collapses `cwnd`.
//! - Never entering `ProbeRtt` and never decaying `cwnd` during idle or app-limited gaps.
//! - Applying gentle multiplicative backoff on sustained congestion and smooth ACK recovery.

use std::{
    any::Any,
    sync::Arc,
    time::{Duration, Instant},
};

use quinn::congestion::{Controller, ControllerFactory, ControllerMetrics};
use quinn_proto::RttEstimator;

/// Default initial congestion window (512 KiB).
pub const DEFAULT_INITIAL_WINDOW: u64 = 512 * 1024;
/// Default hard minimum congestion window floor (256 KiB).
pub const DEFAULT_MIN_WINDOW: u64 = 256 * 1024;
/// Default upper congestion window ceiling (8 MiB) to prevent unbounded bufferbloat.
pub const DEFAULT_MAX_WINDOW: u64 = 8 * 1024 * 1024;

const MIN_DATAGRAM_SIZE: u64 = 1200;
const MIN_LOSS_WINDOW: Duration = Duration::from_millis(20);
const MAX_LOSS_WINDOW: Duration = Duration::from_millis(100);
const DEFAULT_RTT_ESTIMATE: Duration = Duration::from_millis(10);

/// Configuration and factory for [`RealtimeVideoController`].
#[derive(Clone, Debug)]
pub struct RealtimeVideoConfig {
    initial_window: u64,
    min_window: u64,
    max_window: u64,
}

impl Default for RealtimeVideoConfig {
    fn default() -> Self {
        Self {
            initial_window: DEFAULT_INITIAL_WINDOW,
            min_window: DEFAULT_MIN_WINDOW,
            max_window: DEFAULT_MAX_WINDOW,
        }
    }
}

impl RealtimeVideoConfig {
    /// Creates a new configuration with explicit window bounds (in bytes).
    #[cfg(test)]
    pub fn new(initial_window: u64, min_window: u64, max_window: u64) -> Self {
        let min_window = min_window.max(4 * MIN_DATAGRAM_SIZE);
        let max_window = max_window.max(min_window);
        let initial_window = initial_window.clamp(min_window, max_window);
        Self {
            initial_window,
            min_window,
            max_window,
        }
    }
}

impl ControllerFactory for RealtimeVideoConfig {
    fn build(self: Arc<Self>, _now: Instant, current_mtu: u16) -> Box<dyn Controller> {
        Box::new(RealtimeVideoController::new(self, current_mtu))
    }
}

/// Real-time low-latency congestion controller for host-to-viewer video streaming.
#[derive(Clone, Debug)]
pub struct RealtimeVideoController {
    config: Arc<RealtimeVideoConfig>,
    window: u64,
    max_datagram_size: u64,
    smoothed_rtt: Option<Duration>,
    min_rtt: Option<Duration>,
    /// Start of the active recovery epoch; losses for packets sent at or before this
    /// timestamp belong to the same flight and do not trigger duplicate reductions.
    recovery_start: Option<Instant>,
    /// Sliding loss observation epoch to distinguish isolated Wi-Fi packet loss from
    /// sustained congestion.
    loss_epoch_start: Option<Instant>,
    loss_epoch_bytes: u64,
    consecutive_loss_rounds: u32,
}

impl RealtimeVideoController {
    pub fn new(config: Arc<RealtimeVideoConfig>, current_mtu: u16) -> Self {
        let window = config
            .initial_window
            .clamp(config.min_window, config.max_window);
        Self {
            config,
            window,
            max_datagram_size: u64::from(current_mtu).max(MIN_DATAGRAM_SIZE),
            smoothed_rtt: None,
            min_rtt: None,
            recovery_start: None,
            loss_epoch_start: None,
            loss_epoch_bytes: 0,
            consecutive_loss_rounds: 0,
        }
    }

    fn loss_window_duration(&self) -> Duration {
        let rtt = self.smoothed_rtt.unwrap_or(DEFAULT_RTT_ESTIMATE);
        rtt.saturating_mul(2)
            .clamp(MIN_LOSS_WINDOW, MAX_LOSS_WINDOW)
    }

    /// Processes an ACK sample directly with an explicit RTT measurement.
    ///
    /// Separated from [`Controller::on_ack`] so unit tests can exercise ACK recovery
    /// without needing `quinn_proto::RttEstimator::new` (which is `pub(crate)`).
    pub(crate) fn record_ack(
        &mut self,
        _now: Instant,
        sent: Instant,
        bytes: u64,
        app_limited: bool,
        rtt: Duration,
    ) {
        if !rtt.is_zero() {
            self.smoothed_rtt = Some(rtt);
            self.min_rtt = Some(self.min_rtt.map_or(rtt, |prev| prev.min(rtt)));
        }

        // Once packets sent after the last congestion event are acknowledged, we have
        // exited recovery and can clear the consecutive loss streak.
        if self.recovery_start.is_some_and(|start| sent > start) {
            self.recovery_start = None;
            self.consecutive_loss_rounds = 0;
        }

        if bytes == 0 {
            return;
        }

        // Never decay cwnd while app-limited or idle.
        // If cwnd is below initial_window after a congestion backoff, allow smooth
        // recovery even when app-limited so bursty video streams regain full flight
        // capacity promptly.
        if self.window < self.config.initial_window {
            let additive = ((u128::from(self.max_datagram_size) * u128::from(bytes))
                / u128::from(self.window.max(1))) as u64;
            let recovery_step = additive.max(bytes / 4).max(1);
            self.window = self
                .window
                .saturating_add(recovery_step)
                .min(self.config.initial_window)
                .clamp(self.config.min_window, self.config.max_window);
            return;
        }

        if app_limited {
            return;
        }

        // Smooth congestion-avoidance growth (~1 MSS per RTT) above initial_window.
        let increase = ((u128::from(self.max_datagram_size) * u128::from(bytes))
            / u128::from(self.window.max(1))) as u64;
        self.window = self
            .window
            .saturating_add(increase.max(1))
            .clamp(self.config.min_window, self.config.max_window);
    }
}

impl Controller for RealtimeVideoController {
    fn on_sent(&mut self, _now: Instant, _bytes: u64, _last_packet_number: u64) {
        // Intentionally no idle decay or ProbeRtt state transitions.
    }

    fn on_ack(
        &mut self,
        now: Instant,
        sent: Instant,
        bytes: u64,
        app_limited: bool,
        rtt: &RttEstimator,
    ) {
        self.record_ack(now, sent, bytes, app_limited, rtt.get());
    }

    fn on_end_acks(
        &mut self,
        _now: Instant,
        _in_flight: u64,
        _app_limited: bool,
        _largest_packet_num_acked: Option<u64>,
    ) {
        // Intentionally never enters ProbeRtt and never shrinks cwnd during idle periods.
    }

    fn on_congestion_event(
        &mut self,
        now: Instant,
        sent: Instant,
        is_persistent_congestion: bool,
        lost_bytes: u64,
    ) {
        // Reset the short loss-observation window once it ages out.
        let loss_window = self.loss_window_duration();
        if self
            .loss_epoch_start
            .is_none_or(|start| now.saturating_duration_since(start) > loss_window)
        {
            self.loss_epoch_start = Some(now);
            self.loss_epoch_bytes = 0;
        }
        self.loss_epoch_bytes = self.loss_epoch_bytes.saturating_add(lost_bytes);

        if is_persistent_congestion {
            self.recovery_start = Some(now);
            self.consecutive_loss_rounds = self.consecutive_loss_rounds.saturating_add(1);
            // Gentle 0.70x reduction even on persistent congestion, always respecting the
            // hard minimum floor so the next keyframe/burst never stalls.
            self.window = ((u128::from(self.window) * 70) / 100) as u64;
            self.window = self
                .window
                .clamp(self.config.min_window, self.config.max_window);
            return;
        }

        // Ignore additional loss notifications from packets sent in the same flight
        // already accounted for by the active recovery epoch.
        if self.recovery_start.is_some_and(|start| sent <= start) {
            return;
        }

        self.recovery_start = Some(now);
        self.consecutive_loss_rounds = self.consecutive_loss_rounds.saturating_add(1);

        // Distinguish isolated Wi-Fi packet loss (1-2 packets in isolation) from
        // sustained congestion (multiple loss rounds, larger loss burst, or ECN).
        let isolated_loss_threshold = self.max_datagram_size.saturating_mul(2);
        let factor_pct: u128 = if lost_bytes == 0 {
            // ECN CE signal: gentle 0.85x backoff.
            85
        } else if self.consecutive_loss_rounds == 1
            && self.loss_epoch_bytes <= isolated_loss_threshold
        {
            // Isolated 1-2 packet Wi-Fi loss: minimal 0.95x trim so video flow stays smooth.
            95
        } else {
            // Sustained congestion across flights or multi-packet burst loss: smooth 0.80x
            // multiplicative backoff (far gentler than Reno's 0.50x).
            80
        };

        self.window = ((u128::from(self.window) * factor_pct) / 100) as u64;
        self.window = self
            .window
            .clamp(self.config.min_window, self.config.max_window);
    }

    fn on_mtu_update(&mut self, new_mtu: u16) {
        self.max_datagram_size = u64::from(new_mtu).max(MIN_DATAGRAM_SIZE);
        self.window = self
            .window
            .clamp(self.config.min_window, self.config.max_window);
    }

    fn window(&self) -> u64 {
        self.window
    }

    fn metrics(&self) -> ControllerMetrics {
        let mut metrics = ControllerMetrics::default();
        metrics.congestion_window = self.window;
        metrics.ssthresh = Some(self.config.initial_window);
        if let Some(rtt) = self.smoothed_rtt.or(self.min_rtt)
            && !rtt.is_zero()
        {
            // bits/sec = window_bytes * 8 / rtt_secs
            let bps = ((u128::from(self.window) * 8 * 1_000_000_000) / rtt.as_nanos().max(1))
                .min(u128::from(u64::MAX)) as u64;
            metrics.pacing_rate = Some(bps);
        }
        metrics
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        self.config.initial_window
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_controller() -> RealtimeVideoController {
        RealtimeVideoController::new(Arc::new(RealtimeVideoConfig::default()), 1200)
    }

    #[test]
    fn fifteen_second_idle_followed_by_burst_does_not_collapse_cwnd() {
        let mut cc = make_controller();
        let t0 = Instant::now();
        assert_eq!(cc.initial_window(), DEFAULT_INITIAL_WINDOW);
        assert_eq!(cc.window(), DEFAULT_INITIAL_WINDOW);

        // Warm up with normal LAN traffic (2 ms RTT).
        let rtt = Duration::from_millis(2);
        for i in 0..20u64 {
            let sent = t0 + Duration::from_millis(i * 16);
            let acked = sent + rtt;
            cc.on_sent(sent, 1200, i);
            cc.record_ack(acked, sent, 1200, true, rtt);
            cc.on_end_acks(acked, 0, true, Some(i));
        }
        let before_idle = cc.window();
        assert!(before_idle >= DEFAULT_INITIAL_WINDOW);

        // Simulate 15 seconds of static desktop silence (past BBR's 10s ProbeRtt trigger),
        // with occasional keepalive ACKs that do NOT produce a lower min_rtt.
        for sec in 1..=15u64 {
            let sent = t0 + Duration::from_secs(sec);
            let acked = sent + Duration::from_millis(4);
            cc.on_sent(sent, 64, 100 + sec);
            cc.record_ack(acked, sent, 64, true, Duration::from_millis(4));
            cc.on_end_acks(acked, 0, true, Some(100 + sec));
            assert_eq!(
                cc.window(),
                before_idle,
                "cwnd must not collapse or decay during idle at t=+{sec}s"
            );
        }

        // Sudden 180 KiB scene-change burst after 15s of silence must fit in one flight.
        let burst_time = t0 + Duration::from_secs(15) + Duration::from_millis(50);
        for pkt in 0..150u64 {
            cc.on_sent(burst_time, 1200, 1000 + pkt);
        }
        assert!(
            cc.window() >= DEFAULT_INITIAL_WINDOW,
            "cwnd ({}) must remain >= initial window ({}) on burst after 15s idle",
            cc.window(),
            DEFAULT_INITIAL_WINDOW
        );
        assert!(
            cc.window() >= 180 * 1024,
            "180 KiB burst frame must fit in a single flight without ProbeRtt stall"
        );
    }

    #[test]
    fn isolated_wifi_packet_loss_stays_well_above_floor_and_recovers() {
        let mut cc = make_controller();
        let t0 = Instant::now();
        let rtt = Duration::from_millis(3);

        // Single isolated lost packet (1200 bytes) — typical Wi-Fi interference.
        let sent = t0;
        let loss_time = t0 + Duration::from_millis(10);
        cc.on_congestion_event(loss_time, sent, false, 1200);

        let after_isolated = cc.window();
        assert!(
            after_isolated >= (DEFAULT_INITIAL_WINDOW * 90) / 100,
            "isolated loss should barely reduce cwnd, got {after_isolated}"
        );
        assert!(
            after_isolated > DEFAULT_MIN_WINDOW,
            "isolated loss must stay well above min floor ({DEFAULT_MIN_WINDOW}), got {after_isolated}"
        );

        // Additional lost packet from the SAME flight (sent <= recovery_start) must not
        // double-penalize cwnd.
        cc.on_congestion_event(loss_time + Duration::from_millis(1), sent, false, 1200);
        assert_eq!(cc.window(), after_isolated);

        // Subsequent ACKs smoothly recover back to initial_window even when app-limited.
        for i in 1..=120u64 {
            let s = loss_time + Duration::from_millis(i * 4);
            let a = s + rtt;
            cc.record_ack(a, s, 1200, true, rtt);
        }
        assert_eq!(cc.window(), DEFAULT_INITIAL_WINDOW);
    }

    #[test]
    fn sustained_loss_backs_off_smoothly_and_respects_hard_floor() {
        let mut cc = make_controller();
        let t0 = Instant::now();

        let mut windows = vec![cc.window()];
        // Simulate 10 consecutive flights suffering multi-packet congestion loss.
        for round in 1..=10u64 {
            let sent = t0 + Duration::from_millis(round * 30);
            let now = sent + Duration::from_millis(15);
            cc.on_congestion_event(now, sent, false, 6 * 1200);
            windows.push(cc.window());
        }

        // Verify monotonic non-increasing backoff, no single step drops more than 25%,
        // and never drops below DEFAULT_MIN_WINDOW.
        for pair in windows.windows(2) {
            let prev = pair[0];
            let next = pair[1];
            assert!(
                next <= prev,
                "cwnd should not increase on loss: {prev} -> {next}"
            );
            assert!(
                next >= DEFAULT_MIN_WINDOW,
                "cwnd ({next}) violated hard floor ({DEFAULT_MIN_WINDOW})"
            );
            assert!(
                next >= (prev * 75) / 100 || next == DEFAULT_MIN_WINDOW,
                "backoff step too harsh: {prev} -> {next}"
            );
        }
        assert_eq!(*windows.last().unwrap(), DEFAULT_MIN_WINDOW);

        // Even persistent congestion must never break the hard floor.
        let p_sent = t0 + Duration::from_secs(2);
        cc.on_congestion_event(p_sent + Duration::from_millis(50), p_sent, true, 50_000);
        assert_eq!(cc.window(), DEFAULT_MIN_WINDOW);

        // Smooth ACK recovery brings cwnd back up from the floor.
        let rtt = Duration::from_millis(5);
        let mut prev = cc.window();
        for i in 1..=400u64 {
            let s = p_sent + Duration::from_millis(100 + i * 5);
            let a = s + rtt;
            cc.record_ack(a, s, 4 * 1200, false, rtt);
            assert!(cc.window() >= prev);
            prev = cc.window();
        }
        assert!(
            cc.window() >= DEFAULT_INITIAL_WINDOW,
            "expected smooth recovery to >= initial window, got {}",
            cc.window()
        );
    }

    #[test]
    fn controller_factory_and_mtu_update_work() {
        let factory = Arc::new(RealtimeVideoConfig::default());
        let mut boxed = factory.build(Instant::now(), 1450);
        assert_eq!(boxed.initial_window(), DEFAULT_INITIAL_WINDOW);
        assert_eq!(boxed.window(), DEFAULT_INITIAL_WINDOW);
        boxed.on_mtu_update(1200);
        assert_eq!(boxed.window(), DEFAULT_INITIAL_WINDOW);
        let cloned = boxed.clone_box();
        assert_eq!(cloned.window(), boxed.window());
        assert!(
            cloned
                .into_any()
                .downcast::<RealtimeVideoController>()
                .is_ok()
        );

        let custom = Arc::new(RealtimeVideoConfig::new(
            600 * 1024,
            300 * 1024,
            1024 * 1024,
        ));
        let custom_cc = custom.build(Instant::now(), 1200);
        assert_eq!(custom_cc.initial_window(), 600 * 1024);
        assert_eq!(custom_cc.window(), 600 * 1024);
    }
}
