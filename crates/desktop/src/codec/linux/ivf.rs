//! Minimal IVF framing helpers used by the Linux FFmpeg AV1 encoder/decoder path.

use remoters_core::video::Codec;

pub(super) fn ivf_file_header(width: u16, height: u16, fps: u16) -> [u8; 32] {
    let mut header = [0u8; 32];
    header[0..4].copy_from_slice(b"DKIF");
    header[4..6].copy_from_slice(&0u16.to_le_bytes());
    header[6..8].copy_from_slice(&32u16.to_le_bytes());
    header[8..12].copy_from_slice(b"AV01");
    header[12..14].copy_from_slice(&width.to_le_bytes());
    header[14..16].copy_from_slice(&height.to_le_bytes());
    header[16..20].copy_from_slice(&u32::from(fps.max(1)).to_le_bytes());
    header[20..24].copy_from_slice(&1u32.to_le_bytes());
    header
}

pub(super) fn ivf_frame_header(byte_len: u32, pts: u64) -> [u8; 12] {
    let mut header = [0u8; 12];
    header[0..4].copy_from_slice(&byte_len.to_le_bytes());
    header[4..12].copy_from_slice(&pts.to_le_bytes());
    header
}

pub(super) struct IvfReader<R> {
    source: R,
    header_read: bool,
    sequence: u64,
}

impl<R: std::io::Read> IvfReader<R> {
    pub(super) fn new(source: R) -> Self {
        Self {
            source,
            header_read: false,
            sequence: 0,
        }
    }

    pub(super) fn next_unit(&mut self) -> anyhow::Result<Option<remoters_core::video::AccessUnit>> {
        if !self.header_read {
            let mut file_header = [0u8; 32];
            match read_exact_or_eof(&mut self.source, &mut file_header)? {
                false => return Ok(None),
                true => {
                    anyhow::ensure!(
                        &file_header[0..4] == b"DKIF",
                        "Invalid IVF stream header from ffmpeg AV1 encoder."
                    );
                    self.header_read = true;
                }
            }
        }
        let mut frame_header = [0u8; 12];
        if !read_exact_or_eof(&mut self.source, &mut frame_header)? {
            return Ok(None);
        }
        let size = u32::from_le_bytes(frame_header[0..4].try_into().unwrap()) as usize;
        anyhow::ensure!(
            (1..=remoters_core::video::MAX_ACCESS_UNIT).contains(&size),
            "Invalid AV1 IVF frame size: {size}."
        );
        let mut payload = vec![0u8; size];
        self.source.read_exact(&mut payload)?;
        let unit = remoters_core::video::AccessUnit::from_annex_b(
            Codec::Av1,
            self.sequence,
            payload.into(),
        )?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("AV1 video sequence overflow."))?;
        Ok(Some(unit))
    }
}

fn read_exact_or_eof<R: std::io::Read>(source: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut offset = 0;
    while offset < buf.len() {
        match source.read(&mut buf[offset..]) {
            Ok(0) => {
                if offset == 0 {
                    return Ok(false);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Truncated IVF header from ffmpeg.",
                ));
            }
            Ok(n) => offset += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(true)
}
