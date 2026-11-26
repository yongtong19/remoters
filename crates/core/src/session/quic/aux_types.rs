use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::sync::Notify;

pub(super) const MAX_RELATIVE_PATH_BYTES: usize = 1024;
pub(super) const HOST_AUDIO_QUEUE_CAPACITY: usize = 4;
pub(super) const VIEWER_AUDIO_CHANNEL_CAPACITY: usize = 8;

pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_FILE_TRANSFER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_CURSOR_DIMENSION: u16 = 128;
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u8 = 2;
pub const MAX_AUDIO_SAMPLES: usize = 9_600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ClipboardImageFormat {
    Png = 1,
    Jpeg = 2,
    Gif = 3,
    Webp = 4,
    Bmp = 5,
    Svg = 6,
    Tiff = 7,
}

impl ClipboardImageFormat {
    pub(super) fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Png),
            2 => Some(Self::Jpeg),
            3 => Some(Self::Gif),
            4 => Some(Self::Webp),
            5 => Some(Self::Bmp),
            6 => Some(Self::Svg),
            7 => Some(Self::Tiff),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardData {
    Text(String),
    Image {
        format: ClipboardImageFormat,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedFile {
    pub relative_path: String,
    pub saved_path: PathBuf,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum CursorShape {
    #[default]
    Arrow = 0,
    IBeam = 1,
    Crosshair = 2,
    ClosedHand = 3,
    OpenHand = 4,
    PointingHand = 5,
    ResizeLeft = 6,
    ResizeRight = 7,
    ResizeLeftRight = 8,
    ResizeUp = 9,
    ResizeDown = 10,
    ResizeUpDown = 11,
    ResizeUpLeftDownRight = 12,
    ResizeUpRightDownLeft = 13,
    ResizeColumn = 14,
    ResizeRow = 15,
    IBeamVertical = 16,
    OperationNotAllowed = 17,
    DragLink = 18,
    DragCopy = 19,
    ContextualMenu = 20,
    Hidden = 21,
}

impl CursorShape {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Arrow),
            1 => Some(Self::IBeam),
            2 => Some(Self::Crosshair),
            3 => Some(Self::ClosedHand),
            4 => Some(Self::OpenHand),
            5 => Some(Self::PointingHand),
            6 => Some(Self::ResizeLeft),
            7 => Some(Self::ResizeRight),
            8 => Some(Self::ResizeLeftRight),
            9 => Some(Self::ResizeUp),
            10 => Some(Self::ResizeDown),
            11 => Some(Self::ResizeUpDown),
            12 => Some(Self::ResizeUpLeftDownRight),
            13 => Some(Self::ResizeUpRightDownLeft),
            14 => Some(Self::ResizeColumn),
            15 => Some(Self::ResizeRow),
            16 => Some(Self::IBeamVertical),
            17 => Some(Self::OperationNotAllowed),
            18 => Some(Self::DragLink),
            19 => Some(Self::DragCopy),
            20 => Some(Self::ContextualMenu),
            21 => Some(Self::Hidden),
            _ => None,
        }
    }
}

impl TryFrom<u8> for CursorShape {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        Self::from_byte(value).with_context(|| format!("Unknown cursor shape: {value}."))
    }
}

impl From<CursorShape> for u8 {
    fn from(shape: CursorShape) -> Self {
        shape as u8
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    pub hotspot_x: u16,
    pub hotspot_y: u16,
    pub rgba: Vec<u8>,
}

impl CursorImage {
    pub fn new(
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
        rgba: Vec<u8>,
    ) -> Result<Self> {
        let image = Self {
            width,
            height,
            hotspot_x,
            hotspot_y,
            rgba,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.width >= 1
                && self.height >= 1
                && self.width <= MAX_CURSOR_DIMENSION
                && self.height <= MAX_CURSOR_DIMENSION,
            "Cursor image dimensions must be between 1x1 and {MAX_CURSOR_DIMENSION}x{MAX_CURSOR_DIMENSION}."
        );
        let expected_len = usize::from(self.width) * usize::from(self.height) * 4;
        ensure!(
            self.rgba.len() == expected_len,
            "Cursor image RGBA buffer length ({}) does not match {}x{}x4 ({expected_len}).",
            self.rgba.len(),
            self.width,
            self.height
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorUpdate {
    pub shape: CursorShape,
    pub custom: Option<CursorImage>,
}

impl CursorUpdate {
    pub const fn new(shape: CursorShape) -> Self {
        Self {
            shape,
            custom: None,
        }
    }

    pub fn with_custom(shape: CursorShape, image: CursorImage) -> Result<Self> {
        image.validate()?;
        Ok(Self {
            shape,
            custom: Some(image),
        })
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(image) = &self.custom {
            image.validate()?;
        }
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        match &self.custom {
            None => Ok(vec![self.shape as u8, 0]),
            Some(image) => {
                let mut out = Vec::with_capacity(10 + image.rgba.len());
                out.push(self.shape as u8);
                out.push(1);
                out.extend_from_slice(&image.width.to_be_bytes());
                out.extend_from_slice(&image.height.to_be_bytes());
                out.extend_from_slice(&image.hotspot_x.to_be_bytes());
                out.extend_from_slice(&image.hotspot_y.to_be_bytes());
                out.extend_from_slice(&image.rgba);
                Ok(out)
            }
        }
    }
}

impl From<CursorShape> for CursorUpdate {
    fn from(shape: CursorShape) -> Self {
        Self::new(shape)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioFrame {
    pub sample_rate: u32,
    pub channels: u8,
    pub pcm: Vec<i16>,
}

impl AudioFrame {
    pub fn new(sample_rate: u32, channels: u8, pcm: Vec<i16>) -> Result<Self> {
        let frame = Self {
            sample_rate,
            channels,
            pcm,
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<()> {
        validate_audio_params(self.sample_rate, self.channels, self.pcm.len())
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let sample_count = u16::try_from(self.pcm.len())
            .context("Audio frame sample count exceeds wire limit.")?;
        let mut out = Vec::with_capacity(8 + self.pcm.len() * 2);
        out.extend_from_slice(&self.sample_rate.to_be_bytes());
        out.push(self.channels);
        out.push(0);
        out.extend_from_slice(&sample_count.to_be_bytes());
        for &sample in &self.pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(out)
    }
}

pub(super) fn validate_audio_params(
    sample_rate: u32,
    channels: u8,
    sample_count: usize,
) -> Result<()> {
    ensure!(
        (8_000..=96_000).contains(&sample_rate),
        "Audio sample rate {sample_rate} must be between 8000 and 96000 Hz."
    );
    ensure!(
        (1..=2).contains(&channels),
        "Audio channel count {channels} must be 1 or 2."
    );
    ensure!(sample_count > 0, "Audio frame PCM buffer cannot be empty.");
    ensure!(
        sample_count <= MAX_AUDIO_SAMPLES,
        "Audio frame sample count ({sample_count}) exceeds maximum ({MAX_AUDIO_SAMPLES})."
    );
    ensure!(
        sample_count % usize::from(channels) == 0,
        "Audio frame sample count ({sample_count}) must be a multiple of channel count ({channels})."
    );
    Ok(())
}

struct AudioQueueState {
    frames: VecDeque<AudioFrame>,
    closed: bool,
}

pub(super) struct AudioSendQueue {
    state: std::sync::Mutex<AudioQueueState>,
    pub(super) notify: Notify,
}

impl AudioSendQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::new(AudioQueueState {
                frames: VecDeque::with_capacity(HOST_AUDIO_QUEUE_CAPACITY),
                closed: false,
            }),
            notify: Notify::new(),
        })
    }

    pub(super) fn push(&self, frame: AudioFrame) -> Result<()> {
        frame.validate()?;
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.closed {
                bail!("Host audio channel closed.");
            }
            if state.frames.len() >= HOST_AUDIO_QUEUE_CAPACITY {
                state.frames.pop_front();
            }
            state.frames.push_back(frame);
        }
        self.notify.notify_one();
        Ok(())
    }

    pub(super) fn pop(&self) -> Option<AudioFrame> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.frames.pop_front()
    }

    pub(super) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        state.frames.clear();
    }
}

pub(super) struct AudioQueueGuard(pub(super) Arc<AudioSendQueue>);

impl Drop for AudioQueueGuard {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[derive(Debug)]
pub enum SessionAuxiliary {
    Clipboard(ClipboardData),
    FileReceived(ReceivedFile),
    Cursor(CursorUpdate),
}

pub fn sanitize_relative_path(raw: &str) -> Result<PathBuf> {
    ensure!(!raw.is_empty(), "Empty relative path.");
    ensure!(
        raw.len() <= MAX_RELATIVE_PATH_BYTES,
        "Relative path is too long."
    );
    ensure!(!raw.contains('\0'), "Path contains a NUL byte.");
    ensure!(
        !raw.starts_with('/') && !raw.starts_with('\\'),
        "Absolute paths are not allowed."
    );

    let mut clean = PathBuf::new();
    for segment in raw.split(['/', '\\']) {
        ensure!(
            !segment.is_empty() && segment != "." && segment != "..",
            "Invalid relative path segment."
        );
        ensure!(
            !segment
                .chars()
                .any(|ch| { ch < ' ' || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') }),
            "Path contains unsupported characters."
        );
        clean.push(segment);
    }
    ensure!(
        clean
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
            && clean.components().next().is_some(),
        "Invalid relative path."
    );
    Ok(clean)
}

pub fn unique_destination(root: &Path, relative: &Path) -> Result<PathBuf> {
    let candidate = root.join(relative);
    if !candidate.exists() {
        return Ok(candidate);
    }
    let parent = candidate.parent().unwrap_or(root);
    let file_name = candidate
        .file_name()
        .context("Missing file name.")?
        .to_string_lossy();
    let (stem, ext) = match candidate.extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => {
            let stem = candidate
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| file_name.to_string());
            (stem, Some(ext.to_string()))
        }
        _ => (file_name.to_string(), None),
    };
    for index in 1..=10_000u32 {
        let name = match &ext {
            Some(ext) => format!("{stem} ({index}).{ext}"),
            None => format!("{stem} ({index})"),
        };
        let next = parent.join(name);
        if !next.exists() {
            return Ok(next);
        }
    }
    bail!("Too many duplicate files at destination.");
}
