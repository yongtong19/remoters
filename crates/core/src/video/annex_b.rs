use std::io::Read;

use anyhow::{Context, Result, ensure};
use bytes::{Bytes, BytesMut};

use super::{AccessUnit, Codec, MAX_ACCESS_UNIT};

fn start_codes(bytes: &[u8]) -> impl Iterator<Item = (usize, usize)> + '_ {
    bytes
        .windows(3)
        .enumerate()
        .filter(|(_, bytes)| *bytes == [0, 0, 1])
        .map(|(offset, _)| {
            let start = offset - usize::from(offset != 0 && bytes[offset - 1] == 0);
            (start, offset + 3)
        })
}

/// Returns the NAL units of an Annex B access unit without their start codes.
pub fn nal_units(mut bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (_, header) = start_codes(bytes).next()?;
        let end = start_codes(&bytes[header..])
            .next()
            .map_or(bytes.len(), |(start, _)| header + start);
        let nal = &bytes[header..end];
        bytes = &bytes[end..];
        Some(nal)
    })
    .filter(|nal| !nal.is_empty())
}

fn nal_type(codec: Codec, header: u8) -> u8 {
    match codec {
        Codec::H264 => header & 0x1f,
        Codec::Hevc => (header >> 1) & 0x3f,
    }
}

pub(crate) fn inspect(codec: Codec, bytes: &[u8]) -> Result<bool> {
    let mut starts = start_codes(bytes).peekable();
    let mut picture = false;
    let mut idr = false;
    let mut parameters = 0;
    while let Some((offset, header)) = starts.next() {
        let end = starts.peek().map_or(bytes.len(), |(start, _)| *start);
        ensure!(
            end >= header + if codec == Codec::Hevc { 2 } else { 1 } && offset < header,
            "Invalid video NAL unit."
        );
        match (codec, nal_type(codec, bytes[header])) {
            (Codec::H264, 7) | (Codec::Hevc, 32) => parameters |= 1,
            (Codec::H264, 8) | (Codec::Hevc, 33) => parameters |= 2,
            (Codec::Hevc, 34) => parameters |= 4,
            (Codec::H264, 5) | (Codec::Hevc, 19 | 20) => {
                picture = true;
                idr = true;
            }
            (Codec::H264, 1..=4) | (Codec::Hevc, 0..=31) => picture = true,
            _ => {}
        }
    }
    ensure!(picture, "The access unit contains no video picture.");
    Ok(idr && parameters == if codec == Codec::Hevc { 7 } else { 3 })
}

/// Splits an Annex B recording that has an access-unit delimiter before
/// every picture. Live hardware outputs can use `AccessUnit::from_annex_b` directly.
pub fn access_units(codec: Codec, bytes: Bytes) -> impl Iterator<Item = Result<AccessUnit>> {
    let delimiter = delimiter_type(codec);
    let mut offsets: Vec<_> = start_codes(&bytes)
        .filter_map(|(start, header)| {
            (nal_type(codec, *bytes.get(header)?) == delimiter).then_some(start)
        })
        .collect();
    // Include any initial parameter sets before the first delimiter.
    if let Some(first) = offsets.first_mut() {
        *first = 0;
    }
    offsets.push(bytes.len());
    (0..offsets.len().saturating_sub(1)).map(move |index| {
        AccessUnit::from_annex_b(
            codec,
            index as u64,
            bytes.slice(offsets[index]..offsets[index + 1]),
        )
    })
}

/// Reads a live Annex B stream whose encoder emits an AUD before every picture.
/// The following AUD or EOF finishes the current access unit.
pub struct AnnexBReader<R> {
    source: R,
    codec: Codec,
    buffer: BytesMut,
    scan: usize,
    started: bool,
    sequence: u64,
}

impl<R: Read> AnnexBReader<R> {
    pub fn new(source: R, codec: Codec) -> Self {
        Self {
            source,
            codec,
            buffer: BytesMut::new(),
            scan: 0,
            started: false,
            sequence: 0,
        }
    }

    pub fn next_unit(&mut self) -> Result<Option<AccessUnit>> {
        let mut chunk = [0; 16 * 1024];
        loop {
            let marker = start_codes(&self.buffer[self.scan..]).find(|(_, header)| {
                self.buffer
                    .get(self.scan + header)
                    .is_some_and(|&byte| nal_type(self.codec, byte) == delimiter_type(self.codec))
            });
            if let Some((start, header)) = marker {
                if !self.started {
                    self.started = true;
                    self.scan += header + 1;
                    continue;
                }
                let bytes = self.buffer.split_to(self.scan + start).freeze();
                self.scan = header - start + 1;
                return self.unit(bytes).map(Some);
            }
            ensure!(
                self.buffer.len() <= MAX_ACCESS_UNIT + 4,
                "Video access unit exceeds length limit."
            );
            // Keep enough bytes to recognize a delimiter divided by two reads.
            self.scan = self.scan.max(self.buffer.len().saturating_sub(4));
            let count = self.source.read(&mut chunk)?;
            if count == 0 {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                ensure!(
                    self.started,
                    "The encoder stream did not contain access-unit delimiters."
                );
                let bytes = self.buffer.split().freeze();
                self.scan = 0;
                return self.unit(bytes).map(Some);
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }

    fn unit(&mut self, bytes: Bytes) -> Result<AccessUnit> {
        let unit = AccessUnit::from_annex_b(self.codec, self.sequence, bytes)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .context("Video sequence overflow.")?;
        Ok(unit)
    }
}

fn delimiter_type(codec: Codec) -> u8 {
    match codec {
        Codec::H264 => 9,
        Codec::Hevc => 35,
    }
}
