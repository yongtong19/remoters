use std::{
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use remoters_core::video::{AccessUnit, Codec, Format};
use screencapturekit::{
    cm::{CMSampleBufferExt, SCFrameStatus},
    cv::CVPixelBuffer,
    prelude::*,
};
use tokio::sync::mpsc;

use super::apple::HardwareEncoder;

struct Frame {
    surface: CVPixelBuffer,
    captured_at: Instant,
}

#[derive(Default)]
struct CaptureQueue {
    newest: Option<Frame>,
    keyframe: bool,
}

#[derive(Default)]
struct Feed {
    queue: Mutex<CaptureQueue>,
    changed: Condvar,
}

#[derive(Clone)]
pub struct KeyframeTrigger {
    feed: Arc<Feed>,
}

impl KeyframeTrigger {
    pub fn request(&self) {
        self.feed.queue.lock().unwrap().keyframe = true;
        self.feed.changed.notify_one();
    }
}

pub struct Encoder {
    feed: Arc<Feed>,
    pub units: mpsc::Receiver<Result<AccessUnit>>,
}

impl Encoder {
    pub fn start(format: Format, monitor: &xcap::Monitor) -> Result<Self> {
        let display_id = monitor.id()?;
        let feed = Arc::new(Feed::default());
        let capture_feed = feed.clone();
        let (output, units) = mpsc::channel(2);
        thread::Builder::new()
            .name("macos-screen-encoder".into())
            .spawn(move || {
                if let Err(error) = capture(format, display_id, capture_feed, &output) {
                    let _ = output.blocking_send(Err(error));
                }
            })?;
        Ok(Self { feed, units })
    }

    pub fn keyframe_trigger(&self) -> KeyframeTrigger {
        KeyframeTrigger {
            feed: self.feed.clone(),
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        self.units.close();
        self.feed.changed.notify_one();
    }
}

fn capture(
    format: Format,
    display_id: u32,
    feed: Arc<Feed>,
    output: &mpsc::Sender<Result<AccessUnit>>,
) -> Result<()> {
    ensure!(
        format.codec == Codec::H264,
        "The macOS desktop encoder requires H.264."
    );
    let ready = Instant::now() + Duration::from_secs(2);
    let display = loop {
        let target = SCShareableContent::get()?
            .displays()
            .into_iter()
            .find(|display| display.display_id() == display_id);
        if let Some(display) = target {
            break display;
        }
        ensure!(
            Instant::now() < ready,
            "The selected display is unavailable to ScreenCaptureKit."
        );
        thread::sleep(Duration::from_millis(50));
    };
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_excluding_windows(&[])
        .build()?;
    let config = SCStreamConfiguration::new()
        .with_width(u32::from(format.width))
        .with_height(u32::from(format.height))
        .with_shows_cursor(true)
        .with_pixel_format(PixelFormat::YCbCr_420f)
        .with_minimum_frame_interval(&CMTime::new(1, i32::from(format.frames_per_second)));
    let frames = feed.clone();
    let mut stream = SCStream::new(&filter, &config)?;
    stream.add_output_handler(
        move |sample: CMSampleBuffer, kind| {
            if kind != SCStreamOutputType::Screen
                || sample.frame_status() != Some(SCFrameStatus::Complete)
            {
                return;
            }
            if let Some(surface) = sample.pixel_buffer() {
                frames.queue.lock().unwrap().newest = Some(Frame {
                    surface,
                    captured_at: Instant::now(),
                });
                frames.changed.notify_one();
            }
        },
        SCStreamOutputType::Screen,
    )?;
    let encoder = HardwareEncoder::new(format)?;
    stream.start_capture()?;
    let result = send_frames(&encoder, &feed, output);
    let _ = stream.stop_capture();
    result
}

fn send_frames(
    encoder: &HardwareEncoder,
    feed: &Feed,
    output: &mpsc::Sender<Result<AccessUnit>>,
) -> Result<()> {
    let mut last = None::<Frame>;
    let started = Instant::now();
    let mut sequence = 0;
    let mut previous_time = None::<u64>;
    let mut recover = true;
    let mut stats = crate::metrics::EncodeStats::new();
    while !output.is_closed() {
        let queue = feed.queue.lock().unwrap();
        let (mut queue, _) = feed
            .changed
            .wait_timeout_while(queue, Duration::from_secs(1), |queue| {
                !output.is_closed() && queue.newest.is_none() && !(queue.keyframe && last.is_some())
            })
            .unwrap();
        if output.is_closed() {
            break;
        }
        let (surface, at, force) = if let Some(frame) = queue.newest.take() {
            last = Some(frame);
            let frame = last.as_ref().unwrap();
            (
                frame.surface.clone(),
                frame.captured_at,
                std::mem::take(&mut queue.keyframe),
            )
        } else if let Some(frame) = &last
            && std::mem::take(&mut queue.keyframe)
        {
            (frame.surface.clone(), Instant::now(), true)
        } else {
            ensure!(
                last.is_some() || started.elapsed() < Duration::from_secs(5),
                "ScreenCaptureKit did not deliver a desktop frame."
            );
            continue;
        };
        drop(queue);

        let ticks = u64::try_from(at.saturating_duration_since(started).as_micros() * 90 / 1000)?;
        let timestamp = previous_time.map_or(ticks, |last| ticks.max(last + 1));
        let encode_start = Instant::now();
        let Some(bytes) = encoder.encode(&surface, timestamp, force || recover)? else {
            continue;
        };
        let encode_us = encode_start.elapsed().as_micros() as u64;
        previous_time = Some(timestamp);
        let unit = AccessUnit::from_annex_b(Codec::H264, sequence, bytes.into())?
            .with_rtp_timestamp(timestamp);
        sequence += 1;
        match output.try_send(Ok(unit)) {
            Ok(()) => {
                recover = false;
                stats.record(encode_us, false);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                recover = true;
                stats.record(encode_us, true);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
    Ok(())
}
