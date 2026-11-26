use std::time::{Duration, Instant};

use remoters_core::{
    account::{AccountUser, AuthSession, DeviceRecord},
    video::Format,
};
use serde::Serialize;

use crate::input::codec_label;

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    error: String,
}

#[derive(Debug, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Serialize)]
struct AuthUserDto {
    id: String,
    username: String,
    display_name: Option<String>,
}

impl From<AccountUser> for AuthUserDto {
    fn from(user: AccountUser) -> Self {
        Self {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
        }
    }
}

#[derive(Debug, Serialize)]
struct AuthResponse {
    ok: bool,
    token: String,
    user: AuthUserDto,
}

#[derive(Debug, Serialize)]
struct DevicesOkResponse {
    ok: bool,
    devices: Vec<DeviceRecord>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscoveredHostDto {
    pub(crate) instance_id: u64,
    pub(crate) address: String,
    pub(crate) host_name: String,
    pub(crate) platform: String,
    pub(crate) display_label: String,
    pub(crate) view_only: bool,
}

#[derive(Debug, Serialize)]
struct DiscoveryOkResponse {
    ok: bool,
    hosts: Vec<DiscoveredHostDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionStatsDto {
    ok: bool,
    width: u16,
    height: u16,
    target_fps: u16,
    codec: &'static str,
    hdr: bool,
    fps: f64,
    video_bps: f64,
    rtt_ms: f64,
    encode_ms: f64,
    e2e_ms: f64,
    lost_packets: u64,
}

pub(crate) fn error_json(error: impl std::fmt::Display) -> String {
    serde_json::to_string(&ErrorResponse {
        ok: false,
        error: format!("{error:#}"),
    })
    .unwrap_or_else(|_| r#"{"ok":false,"error":"Serialization failed."}"#.to_owned())
}

pub(crate) fn auth_success_json(session: AuthSession) -> String {
    serde_json::to_string(&AuthResponse {
        ok: true,
        token: session.token,
        user: session.user.into(),
    })
    .unwrap_or_else(|error| error_json(error))
}

pub(crate) fn devices_success_json(devices: Vec<DeviceRecord>) -> String {
    serde_json::to_string(&DevicesOkResponse { ok: true, devices })
        .unwrap_or_else(|error| error_json(error))
}

pub(crate) fn ok_json() -> String {
    serde_json::to_string(&OkResponse { ok: true }).unwrap_or_else(|_| r#"{"ok":true}"#.to_owned())
}

pub(crate) fn discovery_success_json(hosts: Vec<DiscoveredHostDto>) -> String {
    serde_json::to_string(&DiscoveryOkResponse { ok: true, hosts })
        .unwrap_or_else(|error| error_json(error))
}

pub(crate) struct StatsAccumulator {
    format: Format,
    window_start: Instant,
    frames: u64,
    video_bytes: u64,
    encode_us_sum: u64,
    encode_samples: u64,
    e2e_us_sum: u64,
    e2e_samples: u64,
    last_encode_ms: f64,
    last_e2e_ms: f64,
}

impl StatsAccumulator {
    pub(crate) fn new(format: Format) -> Self {
        Self {
            format,
            window_start: Instant::now(),
            frames: 0,
            video_bytes: 0,
            encode_us_sum: 0,
            encode_samples: 0,
            e2e_us_sum: 0,
            e2e_samples: 0,
            last_encode_ms: 0.0,
            last_e2e_ms: 0.0,
        }
    }

    pub(crate) fn record_frame(&mut self, bytes: usize, host_latency_us: u32, rtt: Duration) {
        self.frames += 1;
        self.video_bytes += bytes as u64;
        let host = u64::from(host_latency_us);
        if host > 0 {
            self.encode_us_sum += host;
            self.encode_samples += 1;
        }
        let one_way_us = (rtt.as_micros() as u64) / 2;
        self.e2e_us_sum += host.saturating_add(one_way_us);
        self.e2e_samples += 1;
    }

    pub(crate) fn flush_json(&mut self, now: Instant, rtt: Duration, lost_packets: u64) -> String {
        let elapsed = now.saturating_duration_since(self.window_start);
        let secs = elapsed.as_secs_f64().max(0.001);
        let fps = self.frames as f64 / secs;
        let video_bps = (self.video_bytes as f64 * 8.0) / secs;
        if self.encode_samples > 0 {
            self.last_encode_ms = (self.encode_us_sum as f64 / self.encode_samples as f64) / 1000.0;
        }
        if self.e2e_samples > 0 {
            self.last_e2e_ms = (self.e2e_us_sum as f64 / self.e2e_samples as f64) / 1000.0;
        }

        self.window_start = now;
        self.frames = 0;
        self.video_bytes = 0;
        self.encode_us_sum = 0;
        self.encode_samples = 0;
        self.e2e_us_sum = 0;
        self.e2e_samples = 0;

        let dto = SessionStatsDto {
            ok: true,
            width: self.format.width,
            height: self.format.height,
            target_fps: self.format.frames_per_second,
            codec: codec_label(self.format.codec),
            hdr: self.format.hdr,
            fps,
            video_bps,
            rtt_ms: rtt.as_secs_f64() * 1000.0,
            encode_ms: self.last_encode_ms,
            e2e_ms: self.last_e2e_ms,
            lost_packets,
        };
        serde_json::to_string(&dto).unwrap_or_else(|error| error_json(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoters_core::video::Codec;
    use serde_json::Value;

    #[test]
    fn account_and_discovery_json_serialization() {
        let err: Value = serde_json::from_str(&error_json("bad password")).unwrap();
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"], "bad password");

        let ok: Value = serde_json::from_str(&ok_json()).unwrap();
        assert_eq!(ok["ok"], true);

        let auth = auth_success_json(AuthSession {
            token: "tok_123".into(),
            user: AccountUser {
                id: "u1".into(),
                username: "alice".into(),
                display_name: Some("Alice".into()),
                created_at_unix_ms: 42,
            },
            expires_at_unix_ms: None,
        });
        let auth_val: Value = serde_json::from_str(&auth).unwrap();
        assert_eq!(auth_val["ok"], true);
        assert_eq!(auth_val["token"], "tok_123");
        assert_eq!(auth_val["user"]["id"], "u1");
        assert_eq!(auth_val["user"]["username"], "alice");
        assert_eq!(auth_val["user"]["display_name"], "Alice");

        let devices = devices_success_json(vec![DeviceRecord {
            device_id: "dev1".into(),
            name: "Studio Mac".into(),
            platform: "macOS".into(),
            host_address: Some("192.168.1.50:47821".into()),
            pairing_code: Some("abc".into()),
            sharing: true,
            view_only: false,
            display_label: "2560x1440".into(),
            last_seen_unix_ms: 1700000000,
            online: true,
        }]);
        let dev_val: Value = serde_json::from_str(&devices).unwrap();
        assert_eq!(dev_val["ok"], true);
        assert_eq!(dev_val["devices"][0]["device_id"], "dev1");
        assert_eq!(dev_val["devices"][0]["online"], true);

        let disc = discovery_success_json(vec![DiscoveredHostDto {
            instance_id: 99,
            address: "192.168.1.20:47821".into(),
            host_name: "Gaming-PC".into(),
            platform: "Windows".into(),
            display_label: "Primary".into(),
            view_only: true,
        }]);
        let disc_val: Value = serde_json::from_str(&disc).unwrap();
        assert_eq!(disc_val["ok"], true);
        assert_eq!(disc_val["hosts"][0]["instance_id"], 99);
        assert_eq!(disc_val["hosts"][0]["view_only"], true);
    }

    #[test]
    fn stats_json_uses_expected_camel_case_fields() {
        let mut stats = StatsAccumulator::new(Format {
            codec: Codec::Hevc,
            width: 1920,
            height: 1080,
            frames_per_second: 60,
            hdr: false,
        });
        let t0 = stats.window_start;
        stats.record_frame(125_000, 4_500, Duration::from_millis(12));
        let json = stats.flush_json(
            t0 + Duration::from_millis(500),
            Duration::from_millis(12),
            3,
        );
        let parsed: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["width"], 1920);
        assert_eq!(parsed["height"], 1080);
        assert_eq!(parsed["targetFps"], 60);
        assert_eq!(parsed["codec"], "HEVC");
        assert_eq!(parsed["hdr"], false);
        assert_eq!(parsed["lostPackets"], 3);
        assert!((parsed["fps"].as_f64().unwrap() - 2.0).abs() < 0.01);
        assert!((parsed["videoBps"].as_f64().unwrap() - 2_000_000.0).abs() < 1.0);
        assert!((parsed["rttMs"].as_f64().unwrap() - 12.0).abs() < 0.01);
        assert!((parsed["encodeMs"].as_f64().unwrap() - 4.5).abs() < 0.01);
        assert!((parsed["e2eMs"].as_f64().unwrap() - 10.5).abs() < 0.01);

        let mut av1_hdr_stats = StatsAccumulator::new(Format {
            codec: Codec::Av1,
            width: 2400,
            height: 1080,
            frames_per_second: 120,
            hdr: true,
        });
        let t1 = av1_hdr_stats.window_start;
        av1_hdr_stats.record_frame(200_000, 2_000, Duration::from_millis(6));
        let av1_json =
            av1_hdr_stats.flush_json(t1 + Duration::from_millis(500), Duration::from_millis(6), 0);
        let av1_parsed: Value = serde_json::from_str(&av1_json).unwrap();
        assert_eq!(av1_parsed["codec"], "AV1");
        assert_eq!(av1_parsed["hdr"], true);
    }
}
