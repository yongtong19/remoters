//! Linux video decoder and GPUI `Surface` wrapper (CUVID / software FFmpeg).

use std::{
    io::{Read, Write},
    process::{Child, ChildStdin},
    sync::{Arc, mpsc as std_mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use remoters_core::video::{Codec, Format, ReceivedFrame};

use super::{
    ffmpeg_args::{LinuxDecodeBackend, build_decoder_args_for, spawn_ffmpeg},
    ivf::{ivf_file_header, ivf_frame_header},
};

const DECODER_READ_TIMEOUT: Duration = Duration::from_millis(120);
const DECODER_WARMUP_TIMEOUT: Duration = Duration::from_millis(5);

#[derive(Clone, Debug)]
pub struct Surface(pub Arc<gpui::RenderImage>);

impl Surface {
    #[allow(dead_code)]
    pub fn new(image: Arc<gpui::RenderImage>) -> Self {
        Self(image)
    }

    pub fn from_bgra(width: u32, height: u32, bgra: Vec<u8>) -> Result<Self> {
        let buffer = image::RgbaImage::from_raw(width, height, bgra)
            .context("Decoded BGRA frame buffer size did not match video dimensions.")?;
        let frame = image::Frame::new(buffer);
        Ok(Self(Arc::new(gpui::RenderImage::new(smallvec::smallvec![
            frame
        ]))))
    }

    #[allow(dead_code)]
    pub fn as_render_image(&self) -> &Arc<gpui::RenderImage> {
        &self.0
    }

    pub fn into_inner(self) -> Arc<gpui::RenderImage> {
        self.0
    }
}

struct DecoderProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: std_mpsc::Receiver<Result<crate::platform::VideoFrame>>,
    reader: Option<JoinHandle<()>>,
    codec: Codec,
    width: u32,
    height: u32,
    submitted: u64,
}

impl DecoderProcess {
    fn start(codec: Codec, width: u32, height: u32) -> Result<Self> {
        ensure!(
            Format::supports_dimensions(width, height),
            "Unsupported decoded video dimensions ({width}x{height})."
        );
        let backend = LinuxDecodeBackend::from_env_for_codec(codec);
        let args = build_decoder_args_for(codec, backend);
        let mut child = spawn_ffmpeg(&args, "video decoding")?;
        let mut stdin = child
            .stdin
            .take()
            .context("Missing ffmpeg decoder stdin.")?;
        if codec == Codec::Av1 {
            let header = ivf_file_header(width as u16, height as u16, 60);
            stdin
                .write_all(&header)
                .context("Failed to write IVF header to ffmpeg AV1 decoder.")?;
        }
        let mut stdout = child
            .stdout
            .take()
            .context("Missing ffmpeg decoder stdout.")?;
        let stderr = child.stderr.take();
        let frame_bytes = usize::try_from(width)?
            .checked_mul(usize::try_from(height)?)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("Decoded frame dimensions overflowed buffer size.")?;

        let (tx, frames) = std_mpsc::sync_channel(2);
        let reader = thread::Builder::new()
            .name("linux-ffmpeg-decode".into())
            .spawn(move || {
                let mut buf = vec![0u8; frame_bytes];
                loop {
                    match stdout.read_exact(&mut buf) {
                        Ok(()) => {
                            let frame = match Surface::from_bgra(width, height, buf.clone()) {
                                Ok(surface) => crate::platform::linux::make_video_frame(
                                    surface.into_inner(),
                                    width,
                                    height,
                                ),
                                Err(err) => {
                                    let _ = tx.send(Err(err));
                                    break;
                                }
                            };
                            if tx.send(Ok(frame)).is_err() {
                                break;
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                            if let Some(mut err_pipe) = stderr {
                                let mut msg = String::new();
                                let _ = err_pipe.read_to_string(&mut msg);
                                let trimmed = msg.trim();
                                if !trimmed.is_empty() {
                                    let _ =
                                        tx.send(Err(anyhow!("ffmpeg decoder exited: {trimmed}")));
                                }
                            }
                            break;
                        }
                        Err(err) => {
                            let _ = tx.send(Err(anyhow!("ffmpeg decoder read failed: {err}")));
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            frames,
            reader: Some(reader),
            codec,
            width,
            height,
            submitted: 0,
        })
    }

    fn submit(&mut self, bytes: &[u8]) -> Result<Option<crate::platform::VideoFrame>> {
        let stdin = self
            .stdin
            .as_mut()
            .context("ffmpeg decoder input pipe closed.")?;
        let write_result = if self.codec == Codec::Av1 {
            let len = u32::try_from(bytes.len()).context("AV1 frame exceeds u32 size.")?;
            let frame_header = ivf_frame_header(len, self.submitted);
            stdin
                .write_all(&frame_header)
                .and_then(|()| stdin.write_all(bytes))
                .and_then(|()| stdin.flush())
        } else {
            stdin.write_all(bytes).and_then(|()| stdin.flush())
        };
        if let Err(err) = write_result {
            // Surface any diagnostic stderr captured by the reader thread if available.
            if let Ok(Err(reader_err)) = self.frames.try_recv() {
                return Err(reader_err);
            }
            return Err(anyhow!("Failed to write video packet to ffmpeg: {err}"));
        }
        self.submitted += 1;

        let wait = if self.submitted <= 2 {
            DECODER_WARMUP_TIMEOUT
        } else {
            DECODER_READ_TIMEOUT
        };

        let mut latest = match self.frames.recv_timeout(wait) {
            Ok(result) => Some(result?),
            Err(std_mpsc::RecvTimeoutError::Timeout) => None,
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("ffmpeg decoder process terminated unexpectedly."));
            }
        };
        while let Ok(next) = self.frames.try_recv() {
            latest = Some(next?);
        }
        Ok(latest)
    }
}

impl Drop for DecoderProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[derive(Default)]
pub struct Decoder {
    width: u32,
    height: u32,
    process: Option<DecoderProcess>,
}

impl Decoder {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_format(format: Format) -> Self {
        Self {
            width: u32::from(format.width),
            height: u32::from(format.height),
            process: None,
        }
    }

    #[allow(dead_code)]
    pub fn set_dimensions(&mut self, width: u32, height: u32) {
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.process = None;
        }
    }

    pub fn decode(&mut self, frame: &ReceivedFrame) -> Result<Option<crate::platform::VideoFrame>> {
        if frame.bytes.is_empty() {
            return Ok(None);
        }
        ensure!(
            self.width >= 2 && self.height >= 2,
            "Linux video decoder requires negotiated stream dimensions."
        );
        let needs_restart = self.process.as_ref().is_none_or(|proc_state| {
            proc_state.codec != frame.codec
                || proc_state.width != self.width
                || proc_state.height != self.height
        });
        if needs_restart {
            self.process = None;
            self.process = Some(DecoderProcess::start(frame.codec, self.width, self.height)?);
        }
        let proc_state = self.process.as_mut().unwrap();
        proc_state.submit(&frame.bytes)
    }
}
