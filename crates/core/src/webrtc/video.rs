use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use str0m::{
    Rtc,
    format::Codec as RtcCodec,
    media::{KeyframeRequestKind, MediaData, Mid, Rid},
};
use tokio::sync::mpsc;

use crate::video::{Codec, MAX_ACCESS_UNIT, ReceivedFrame, inspect};

const KEYFRAME_INTERVAL: Duration = Duration::from_millis(250);
const HEVC_ASSEMBLY_TIMEOUT: Duration = Duration::from_millis(32);

pub(super) struct VideoOutput {
    frames: mpsc::Sender<ReceivedFrame>,
    decodable: bool,
    keyframe: Option<(Mid, Option<Rid>)>,
    last_pli: Option<Instant>,
    pending_hevc: Option<HevcPicture>,
    emitted_hevc: Option<u64>,
}

struct HevcPicture {
    timestamp: u64,
    bytes: Vec<u8>,
    track: (Mid, Option<Rid>),
    intact: bool,
    deadline: Instant,
}

impl VideoOutput {
    pub(super) fn new(frames: mpsc::Sender<ReceivedFrame>) -> Self {
        Self {
            frames,
            decodable: false,
            keyframe: None,
            last_pli: None,
            pending_hevc: None,
            emitted_hevc: None,
        }
    }

    pub(super) fn flush_due(&mut self, now: Instant) -> Option<Instant> {
        let deadline = self.pending_hevc.as_ref()?.deadline;
        if deadline > now {
            Some(deadline)
        } else {
            self.flush_hevc();
            None
        }
    }

    pub(super) fn request_keyframe(&mut self, rtc: &mut Rtc, now: Instant) -> bool {
        if self
            .last_pli
            .is_some_and(|last| now - last < KEYFRAME_INTERVAL)
        {
            return false;
        }
        let Some((mid, rid)) = self.keyframe.take() else {
            return false;
        };
        let Some(mut writer) = rtc.writer(mid) else {
            return false;
        };
        self.last_pli = Some(now);
        let _ = writer.request_keyframe(rid, KeyframeRequestKind::Pli);
        true
    }

    pub(super) fn media(&mut self, media: MediaData) -> Result<()> {
        let timestamp = media.time.numer();
        let track = (media.mid, media.rid);
        ensure!(
            media.data.len() <= MAX_ACCESS_UNIT,
            "Video frame exceeds length limit."
        );
        match media.params.spec().codec {
            RtcCodec::H264 => {
                self.decodable &= media.contiguous;
                self.emit(Codec::H264, timestamp, media.data, track);
            }
            RtcCodec::H265 => {
                if self.emitted_hevc.is_some_and(|last| timestamp <= last) {
                    // A slice that arrives after its picture's deadline cannot
                    // be added to the already submitted decoder input.
                    self.decodable = false;
                    self.keyframe = Some(track);
                    return Ok(());
                }
                if let Some(picture) = &mut self.pending_hevc {
                    if picture.timestamp == timestamp {
                        ensure!(
                            picture.bytes.len() + media.data.len() <= MAX_ACCESS_UNIT,
                            "Video frame exceeds length limit."
                        );
                        picture.bytes.extend_from_slice(&media.data);
                        picture.intact &= media.contiguous;
                        picture.deadline = Instant::now() + HEVC_ASSEMBLY_TIMEOUT;
                        return Ok(());
                    }
                    if timestamp < picture.timestamp {
                        return Ok(());
                    }
                    picture.intact &= media.contiguous;
                    self.flush_hevc();
                }
                self.decodable &= media.contiguous;
                // str0m's H.265 depacketizer emits fragmented slices
                // separately even when they share the same RTP timestamp.
                self.pending_hevc = Some(HevcPicture {
                    timestamp,
                    bytes: media.data.to_vec(),
                    track,
                    intact: true,
                    deadline: Instant::now() + HEVC_ASSEMBLY_TIMEOUT,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn flush_hevc(&mut self) {
        let Some(picture) = self.pending_hevc.take() else {
            return;
        };
        self.emitted_hevc = Some(picture.timestamp);
        if picture.intact {
            self.emit(
                Codec::Hevc,
                picture.timestamp,
                picture.bytes.into(),
                picture.track,
            );
        } else {
            self.decodable = false;
            self.keyframe = Some(picture.track);
        }
    }

    fn emit(
        &mut self,
        codec: Codec,
        rtp_timestamp: u64,
        bytes: Arc<[u8]>,
        track: (Mid, Option<Rid>),
    ) {
        if !self.decodable && !inspect(codec, &bytes).unwrap_or(false) {
            self.keyframe = Some(track);
            return;
        }
        self.decodable = self
            .frames
            .try_send(ReceivedFrame {
                codec,
                rtp_timestamp,
                host_latency_us: 0,
                bytes,
            })
            .is_ok();
        self.keyframe = (!self.decodable).then_some(track);
    }
}
