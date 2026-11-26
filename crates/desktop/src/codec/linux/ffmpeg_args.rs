//! Linux FFmpeg encoder/decoder argument builders, low-latency VBV sizing,
//! backend selection, and hardware codec probing.

#[cfg(target_os = "linux")]
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    sync::OnceLock,
};

#[cfg(target_os = "linux")]
use anyhow::{Result, anyhow};
use remoters_core::video::Codec;

#[cfg(target_os = "linux")]
use super::nvenc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LinuxVideoBackend {
    #[default]
    Software,
    Vaapi,
    Nvenc,
    NvencDirect,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LinuxDecodeBackend {
    #[default]
    Software,
    Cuvid,
}

pub fn resolve_backend_from_env(
    env_value: Option<&str>,
    hdr: bool,
    direct_nvenc_ok: bool,
    ffmpeg_nvenc_ok: bool,
) -> LinuxVideoBackend {
    match env_value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("nvenc-direct") => {
            if !hdr && direct_nvenc_ok {
                LinuxVideoBackend::NvencDirect
            } else if ffmpeg_nvenc_ok {
                LinuxVideoBackend::Nvenc
            } else {
                LinuxVideoBackend::Software
            }
        }
        Some("ffmpeg") => {
            if ffmpeg_nvenc_ok {
                LinuxVideoBackend::Nvenc
            } else {
                LinuxVideoBackend::Software
            }
        }
        Some("nvenc") => LinuxVideoBackend::Nvenc,
        Some("vaapi") => LinuxVideoBackend::Vaapi,
        Some("software") => LinuxVideoBackend::Software,
        _ => {
            if !hdr && direct_nvenc_ok {
                LinuxVideoBackend::NvencDirect
            } else if ffmpeg_nvenc_ok {
                LinuxVideoBackend::Nvenc
            } else {
                LinuxVideoBackend::Software
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl LinuxVideoBackend {
    pub fn from_env_for_codec(codec: Codec, hdr: bool) -> Self {
        let env_raw = std::env::var("REMOTERS_LINUX_ENCODER").ok();
        let env_norm = env_raw.as_deref().map(|v| v.trim().to_ascii_lowercase());
        match env_norm.as_deref() {
            Some("nvenc") => Self::Nvenc,
            Some("vaapi") => Self::Vaapi,
            Some("software") => Self::Software,
            Some("ffmpeg") => {
                resolve_backend_from_env(Some("ffmpeg"), hdr, false, probe_nvenc_codec(codec, hdr))
            }
            other => {
                let direct_ok = !hdr && nvenc::probe_direct_nvenc(codec);
                let ffmpeg_ok = if direct_ok {
                    false
                } else {
                    probe_nvenc_codec(codec, hdr)
                };
                resolve_backend_from_env(other, hdr, direct_ok, ffmpeg_ok)
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl LinuxDecodeBackend {
    pub fn from_env_for_codec(codec: Codec) -> Self {
        match std::env::var("REMOTERS_LINUX_DECODER")
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("cuvid") => Self::Cuvid,
            Some("software") => Self::Software,
            _ => {
                if probe_cuvid_codec(codec) {
                    Self::Cuvid
                } else {
                    Self::Software
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EncoderArgsConfig {
    pub codec: Codec,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u16,
    pub output_height: u16,
    pub fps: u16,
    pub bitrate_bps: u32,
    pub hdr: bool,
    pub backend: LinuxVideoBackend,
    pub input_pix_fmt: &'static str,
}

fn apply_hdr_ffmpeg_flags(args: &mut Vec<String>) {
    args.extend([
        "-color_primaries".to_owned(),
        "bt2020".to_owned(),
        "-color_trc".to_owned(),
        "smpte2084".to_owned(),
        "-colorspace".to_owned(),
        "bt2020nc".to_owned(),
        "-color_range".to_owned(),
        "tv".to_owned(),
    ]);
}

/// Computes a strict low-latency VBV buffer size (~2 frames of bitrate, in kbps)
/// with a safe lower floor (250 kbps) so x264/x265/SVT-AV1/VAAPI/NVENC never
/// reject tiny VBV values while preventing multi-second static-to-motion bursts.
pub fn low_latency_vbv_kbps(bitrate_bps: u32, fps: u16) -> u32 {
    let fps = u64::from(fps.max(1));
    let kbps = u64::from((bitrate_bps / 1_000).max(500));
    let two_frames = (kbps * 2).div_ceil(fps);
    two_frames.clamp(250, kbps) as u32
}

pub fn build_encoder_args(config: &EncoderArgsConfig) -> Vec<String> {
    build_encoder_args_with_input(config, None)
}

pub fn build_encoder_args_with_input(
    config: &EncoderArgsConfig,
    x11grab_input: Option<&str>,
) -> Vec<String> {
    let fps = config.fps.max(1);
    let kbps = (config.bitrate_bps / 1_000).max(500);
    let buf_kbps = low_latency_vbv_kbps(config.bitrate_bps, fps);
    let gop = u32::from(fps);
    let out_w = u32::from(config.output_width);
    let out_h = u32::from(config.output_height);
    let needs_scale = config.input_width != out_w || config.input_height != out_h;
    let hdr_enabled = config.hdr && matches!(config.codec, Codec::Hevc | Codec::Av1);

    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
    ];

    if config.backend == LinuxVideoBackend::Vaapi {
        args.push("-vaapi_device".to_owned());
        args.push("/dev/dri/renderD128".to_owned());
    }

    if let Some(display_input) = x11grab_input {
        args.extend([
            "-f".to_owned(),
            "x11grab".to_owned(),
            "-framerate".to_owned(),
            fps.to_string(),
            "-video_size".to_owned(),
            format!("{}x{}", config.input_width, config.input_height),
            "-i".to_owned(),
            display_input.to_owned(),
        ]);
    } else {
        args.extend([
            "-f".to_owned(),
            "rawvideo".to_owned(),
            "-pix_fmt".to_owned(),
            config.input_pix_fmt.to_owned(),
            "-s".to_owned(),
            format!("{}x{}", config.input_width, config.input_height),
            "-r".to_owned(),
            fps.to_string(),
            "-i".to_owned(),
            "pipe:0".to_owned(),
        ]);
    }

    match config.backend {
        LinuxVideoBackend::Software => {
            if needs_scale {
                args.push("-vf".to_owned());
                args.push(format!("scale={out_w}:{out_h}"));
            }
            let sw_pix_fmt = if hdr_enabled {
                "yuv420p10le"
            } else {
                "yuv420p"
            };
            match config.codec {
                Codec::H264 => {
                    args.extend([
                        "-c:v".to_owned(),
                        "libx264".to_owned(),
                        "-pix_fmt".to_owned(),
                        "yuv420p".to_owned(),
                        "-preset".to_owned(),
                        "ultrafast".to_owned(),
                        "-tune".to_owned(),
                        "zerolatency".to_owned(),
                        "-x264-params".to_owned(),
                        "aud=1:repeat-headers=1:bframes=0:scenecut=0".to_owned(),
                    ]);
                }
                Codec::Hevc => {
                    args.extend([
                        "-c:v".to_owned(),
                        "libx265".to_owned(),
                        "-pix_fmt".to_owned(),
                        sw_pix_fmt.to_owned(),
                        "-preset".to_owned(),
                        "ultrafast".to_owned(),
                        "-tune".to_owned(),
                        "zerolatency".to_owned(),
                        "-x265-params".to_owned(),
                        "aud=1:repeat-headers=1:bframes=0:scenecut=0:log-level=error".to_owned(),
                    ]);
                }
                Codec::Av1 => {
                    args.extend([
                        "-c:v".to_owned(),
                        "libsvtav1".to_owned(),
                        "-pix_fmt".to_owned(),
                        sw_pix_fmt.to_owned(),
                        "-preset".to_owned(),
                        "10".to_owned(),
                    ]);
                }
            }
        }
        LinuxVideoBackend::Vaapi => {
            let hw_pix_fmt = if hdr_enabled { "p010le" } else { "nv12" };
            let vf = if needs_scale {
                format!("scale={out_w}:{out_h},format={hw_pix_fmt},hwupload")
            } else {
                format!("format={hw_pix_fmt},hwupload")
            };
            args.push("-vf".to_owned());
            args.push(vf);
            match config.codec {
                Codec::H264 => {
                    args.extend([
                        "-c:v".to_owned(),
                        "h264_vaapi".to_owned(),
                        "-bf".to_owned(),
                        "0".to_owned(),
                        "-bsf:v".to_owned(),
                        "h264_metadata=aud=insert".to_owned(),
                    ]);
                }
                Codec::Hevc => {
                    args.extend([
                        "-c:v".to_owned(),
                        "hevc_vaapi".to_owned(),
                        "-bf".to_owned(),
                        "0".to_owned(),
                        "-bsf:v".to_owned(),
                        "hevc_metadata=aud=insert".to_owned(),
                    ]);
                    if hdr_enabled {
                        args.extend(["-profile:v".to_owned(), "main10".to_owned()]);
                    }
                }
                Codec::Av1 => {
                    args.extend([
                        "-c:v".to_owned(),
                        "av1_vaapi".to_owned(),
                        "-bf".to_owned(),
                        "0".to_owned(),
                    ]);
                }
            }
        }
        LinuxVideoBackend::Nvenc | LinuxVideoBackend::NvencDirect => {
            if needs_scale {
                args.push("-vf".to_owned());
                args.push(format!("scale={out_w}:{out_h}"));
            }
            let nv_pix_fmt = if hdr_enabled {
                "p010le"
            } else if x11grab_input.is_some() {
                "bgr0"
            } else {
                config.input_pix_fmt
            };
            let encoder_name = match config.codec {
                Codec::H264 => "h264_nvenc",
                Codec::Hevc => "hevc_nvenc",
                Codec::Av1 => "av1_nvenc",
            };
            args.extend([
                "-c:v".to_owned(),
                encoder_name.to_owned(),
                "-pix_fmt".to_owned(),
                nv_pix_fmt.to_owned(),
                "-preset".to_owned(),
                "p1".to_owned(),
                "-tune".to_owned(),
                "ull".to_owned(),
                "-zerolatency".to_owned(),
                "1".to_owned(),
                "-rc".to_owned(),
                "cbr".to_owned(),
                "-bf".to_owned(),
                "0".to_owned(),
                "-forced-idr".to_owned(),
                "1".to_owned(),
            ]);
            if matches!(config.codec, Codec::H264 | Codec::Hevc) {
                args.extend([
                    "-aud".to_owned(),
                    "1".to_owned(),
                    "-intra-refresh".to_owned(),
                    "1".to_owned(),
                ]);
            }
            if hdr_enabled && config.codec == Codec::Hevc {
                args.extend(["-profile:v".to_owned(), "main10".to_owned()]);
            }
        }
    }

    if hdr_enabled {
        apply_hdr_ffmpeg_flags(&mut args);
    }

    let container = match config.codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "ivf",
    };

    args.extend([
        "-g".to_owned(),
        gop.to_string(),
        "-b:v".to_owned(),
        format!("{kbps}k"),
        "-maxrate".to_owned(),
        format!("{kbps}k"),
        "-bufsize".to_owned(),
        format!("{buf_kbps}k"),
        "-flush_packets".to_owned(),
        "1".to_owned(),
        "-f".to_owned(),
        container.to_owned(),
        "pipe:1".to_owned(),
    ]);

    args
}

#[allow(dead_code)]
pub fn build_decoder_args(codec: Codec) -> Vec<String> {
    build_decoder_args_for(codec, LinuxDecodeBackend::Software)
}

pub fn build_decoder_args_for(codec: Codec, backend: LinuxDecodeBackend) -> Vec<String> {
    let input_format = match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "ivf",
    };
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-flags".to_owned(),
        "low_delay".to_owned(),
        "-probesize".to_owned(),
        "32".to_owned(),
        "-analyzeduration".to_owned(),
        "0".to_owned(),
        "-fpsprobesize".to_owned(),
        "0".to_owned(),
        "-threads".to_owned(),
        "1".to_owned(),
        "-f".to_owned(),
        input_format.to_owned(),
    ];
    if backend == LinuxDecodeBackend::Cuvid {
        let cuvid = match codec {
            Codec::H264 => "h264_cuvid",
            Codec::Hevc => "hevc_cuvid",
            Codec::Av1 => "av1_cuvid",
        };
        args.extend(["-c:v".to_owned(), cuvid.to_owned()]);
    }
    args.extend([
        "-i".to_owned(),
        "pipe:0".to_owned(),
        "-pix_fmt".to_owned(),
        "bgra".to_owned(),
        "-f".to_owned(),
        "rawvideo".to_owned(),
        "pipe:1".to_owned(),
    ]);
    args
}

#[cfg(target_os = "linux")]
fn ffmpeg_lists_codec(flag: &str, needle: &str) -> bool {
    let Ok(output) = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", flag])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().any(|line| line.contains(needle))
}

#[cfg(target_os = "linux")]
fn probe_ffmpeg_hw_encode_runtime(config: &EncoderArgsConfig) -> bool {
    let args = build_encoder_args(config);
    let Ok(mut child) = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let frame_bytes = (config.input_width as usize) * (config.input_height as usize) * 4;
    if let Some(mut stdin) = child.stdin.take() {
        let zero = vec![0u8; frame_bytes];
        let _ = stdin.write_all(&zero);
        let _ = stdin.write_all(&zero);
        let _ = stdin.flush();
    }
    child.wait().is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
pub(super) fn probe_nvenc_codec(codec: Codec, hdr: bool) -> bool {
    let name = match codec {
        Codec::H264 => "h264_nvenc",
        Codec::Hevc => "hevc_nvenc",
        Codec::Av1 => "av1_nvenc",
    };
    if !ffmpeg_lists_codec("-encoders", name) {
        return false;
    }
    probe_ffmpeg_hw_encode_runtime(&EncoderArgsConfig {
        codec,
        input_width: 256,
        input_height: 256,
        output_width: 256,
        output_height: 256,
        fps: 60,
        bitrate_bps: 5_000_000,
        hdr: hdr && matches!(codec, Codec::Hevc | Codec::Av1),
        backend: LinuxVideoBackend::Nvenc,
        input_pix_fmt: "rgba",
    })
}

#[cfg(target_os = "linux")]
pub(super) fn probe_cuvid_codec(codec: Codec) -> bool {
    let cuvid_name = match codec {
        Codec::H264 => "h264_cuvid",
        Codec::Hevc => "hevc_cuvid",
        Codec::Av1 => "av1_cuvid",
    };
    if !ffmpeg_lists_codec("-decoders", cuvid_name) {
        return false;
    }
    // Confirm an NVIDIA GPU / NVDEC runtime is actually reachable via nvidia-smi.
    Command::new("nvidia-smi")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(target_os = "linux")]
pub fn probe_av1_encoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        nvenc::probe_direct_nvenc(Codec::Av1)
            || probe_nvenc_codec(Codec::Av1, false)
            || ffmpeg_lists_codec("-encoders", "av1_vaapi")
            || ffmpeg_lists_codec("-encoders", "libsvtav1")
    })
}

#[cfg(target_os = "linux")]
pub fn probe_av1_decoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_cuvid_codec(Codec::Av1)
            || ffmpeg_lists_codec("-decoders", "libdav1d")
            || ffmpeg_lists_codec("-decoders", "av1")
    })
}

#[cfg(target_os = "linux")]
pub fn probe_hevc_encoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        nvenc::probe_direct_nvenc(Codec::Hevc)
            || probe_nvenc_codec(Codec::Hevc, false)
            || ffmpeg_lists_codec("-encoders", "hevc_vaapi")
            || ffmpeg_lists_codec("-encoders", "libx265")
    })
}

#[cfg(target_os = "linux")]
pub fn probe_hevc_decoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED
        .get_or_init(|| probe_cuvid_codec(Codec::Hevc) || ffmpeg_lists_codec("-decoders", "hevc"))
}

#[cfg(target_os = "linux")]
pub fn probe_hdr_encoder() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        probe_nvenc_codec(Codec::Av1, true)
            || probe_nvenc_codec(Codec::Hevc, true)
            || (ffmpeg_lists_codec("-pix_fmts", "yuv420p10le")
                && (probe_hevc_encoder() || probe_av1_encoder()))
    })
}

#[cfg(target_os = "linux")]
pub(super) fn spawn_ffmpeg(args: &[String], role: &str) -> Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "ffmpeg is required for {role} on Linux. Install it with: sudo apt install ffmpeg"
                )
            } else {
                anyhow!("Failed to start ffmpeg for {role}: {err}")
            }
        })
}
