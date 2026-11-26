//! Systematic GF(2^8) Cauchy Reed-Solomon erasure coding and video datagram reassembly.
//!
//! Video frames that fit within `MAX_DATA_SHARDS` QUIC datagrams are split into `k` data
//! shards and protected by `m` (~20%, minimum 2) systematic Cauchy parity shards. Any
//! subset of `k` out of `k + m` shards reconstructs the original frame in 0-RTT without
//! retransmissions or keyframe requests.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
    time::{Duration, Instant},
};

use anyhow::{Result, bail, ensure};
use bytes::{BufMut, Bytes, BytesMut};

use crate::video::MAX_ACCESS_UNIT;

/// Distinct 1-byte magic prefix marking host->viewer FEC video datagrams.
///
/// Host->viewer currently carries no other datagrams; viewer->host input datagrams are
/// strictly in the opposite direction and remain untouched.
pub const FEC_VIDEO_MAGIC: u8 = 0xFE;

/// Wire header size for every FEC video datagram:
/// `[magic: 1][sequence: 8][timestamp: 8][host_latency_us: 4][frame_len: 4][k: 1][m: 1][idx: 1]`
pub const FEC_HEADER_SIZE: usize = 28;

/// Minimum useful payload bytes per datagram shard before falling back to uni-streams.
pub const MIN_SHARD_PAYLOAD: usize = 64;

/// Maximum data shards per FEC-encoded video frame (~190-200 KiB at typical QUIC MTU).
pub const MAX_DATA_SHARDS: usize = 180;

/// Maximum total shards (`k + m`) representable with distinct GF(2^8) Cauchy coordinates.
pub const MAX_TOTAL_SHARDS: usize = 255;

/// Maximum concurrent incomplete FEC frames buffered on the viewer.
const MAX_INCOMPLETE_FRAMES: usize = 16;

/// Maximum number of recently completed frame sequences remembered to ignore trailing
/// redundant parity shards without recreating incomplete buckets.
const MAX_COMPLETED_SEQUENCES: usize = 256;

/// Time after the first shard arrives before an incomplete FEC frame is considered
/// genuinely unrecoverable.
pub const FEC_FRAME_TIMEOUT: Duration = Duration::from_millis(80);

// ---------------------------------------------------------------------------
// GF(2^8) arithmetic over primitive polynomial x^8 + x^4 + x^3 + x^2 + 1 (0x11D)
// ---------------------------------------------------------------------------

const GF_POLY: u16 = 0x11D;

const fn build_exp_log_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x: u16 = 1;
    let mut i = 0usize;
    while i < 255 {
        exp[i] = x as u8;
        exp[i + 255] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if (x & 0x100) != 0 {
            x ^= GF_POLY;
        }
        i += 1;
    }
    exp[510] = exp[0];
    exp[511] = exp[1];
    (exp, log)
}

const GF_TABLES: ([u8; 512], [u8; 256]) = build_exp_log_tables();
const GF_EXP: &[u8; 512] = &GF_TABLES.0;
const GF_LOG: &[u8; 256] = &GF_TABLES.1;

#[inline]
fn gf_mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        return 0;
    }
    GF_EXP[usize::from(GF_LOG[usize::from(a)]) + usize::from(GF_LOG[usize::from(b)])]
}

#[inline]
fn gf_inv(a: u8) -> u8 {
    debug_assert!(a != 0, "division by zero in GF(2^8)");
    GF_EXP[255 - usize::from(GF_LOG[usize::from(a)])]
}

/// Lazily initialized 256x256 multiplication table for fast branchless slice operations.
fn mul_table() -> &'static [[u8; 256]; 256] {
    static TABLE: OnceLock<Box<[[u8; 256]; 256]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut tbl = Box::new([[0u8; 256]; 256]);
        for a in 1..256usize {
            let log_a = usize::from(GF_LOG[a]);
            let row = &mut tbl[a];
            for (b, slot) in row.iter_mut().enumerate().skip(1) {
                *slot = GF_EXP[log_a + usize::from(GF_LOG[b])];
            }
        }
        tbl
    })
}

/// Computes `dst[i] ^= coeff * src[i]` across the slice in GF(2^8).
#[inline]
fn gf_mul_slice_xor(dst: &mut [u8], src: &[u8], coeff: u8) {
    debug_assert_eq!(dst.len(), src.len());
    if coeff == 0 {
        return;
    }
    if coeff == 1 {
        for (d, &s) in dst.iter_mut().zip(src) {
            *d ^= s;
        }
        return;
    }
    let row = &mul_table()[usize::from(coeff)];
    for (d, &s) in dst.iter_mut().zip(src) {
        *d ^= row[usize::from(s)];
    }
}

/// Cauchy parity matrix entry `P[parity_idx][data_idx] = 1 / ((k + parity_idx) ^ data_idx)`.
///
/// Because `data_idx in 0..k` and `k + parity_idx in k..k+m` with `k + m <= 255`,
/// all coordinates are distinct elements of GF(2^8), guaranteeing that every square
/// submatrix of `P` is strictly invertible.
#[inline]
fn cauchy_coeff(data_shards: usize, parity_idx: usize, data_idx: usize) -> u8 {
    let x = (data_shards + parity_idx) as u8;
    let y = data_idx as u8;
    gf_inv(x ^ y)
}

/// Inverts a square `n x n` matrix over GF(2^8) using Gauss-Jordan elimination.
fn invert_matrix(mut mat: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
    let n = mat.len();
    let mut inv = vec![vec![0u8; n]; n];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1;
    }

    for col in 0..n {
        let pivot = (col..n)
            .find(|&row| mat[row][col] != 0)
            .ok_or_else(|| anyhow::anyhow!("Singular GF(2^8) matrix during RS decode."))?;
        if pivot != col {
            mat.swap(col, pivot);
            inv.swap(col, pivot);
        }

        let inv_diag = gf_inv(mat[col][col]);
        if inv_diag != 1 {
            for v in &mut mat[col] {
                *v = gf_mul(*v, inv_diag);
            }
            for v in &mut inv[col] {
                *v = gf_mul(*v, inv_diag);
            }
        }

        let pivot_mat = mat[col].clone();
        let pivot_inv = inv[col].clone();
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = mat[row][col];
            if factor != 0 {
                gf_mul_slice_xor(&mut mat[row], &pivot_mat, factor);
                gf_mul_slice_xor(&mut inv[row], &pivot_inv, factor);
            }
        }
    }

    Ok(inv)
}

// ---------------------------------------------------------------------------
// Systematic Reed-Solomon shard encoding & erasure decoding
// ---------------------------------------------------------------------------

/// Computes the number of parity shards (~20% overhead, minimum 2) for `data_shards`.
pub fn parity_shard_count(data_shards: usize) -> usize {
    if data_shards == 0 || data_shards >= MAX_TOTAL_SHARDS {
        return 0;
    }
    let target = data_shards.div_ceil(5).max(2);
    target.min(MAX_TOTAL_SHARDS - data_shards)
}

/// Returns whether a frame of `frame_len` bytes fits within FEC datagram limits for
/// `max_datagram_size`.
pub fn can_encode_frame(frame_len: usize, max_datagram_size: usize) -> bool {
    if frame_len == 0 || frame_len > MAX_ACCESS_UNIT {
        return false;
    }
    let max_payload = max_datagram_size.saturating_sub(FEC_HEADER_SIZE);
    if max_payload < MIN_SHARD_PAYLOAD {
        return false;
    }
    let data_shards = frame_len.div_ceil(max_payload);
    let parity_shards = parity_shard_count(data_shards);
    data_shards >= 1
        && data_shards <= MAX_DATA_SHARDS
        && parity_shards >= 1
        && data_shards + parity_shards <= MAX_TOTAL_SHARDS
}

/// Encodes `data_shards` equal-length slices into `parity_shards` systematic Cauchy
/// parity shards.
pub fn encode_parity_shards(data_shards: &[&[u8]], parity_shards: usize) -> Result<Vec<Vec<u8>>> {
    let k = data_shards.len();
    ensure!(
        (1..=MAX_DATA_SHARDS).contains(&k),
        "Invalid RS data shard count: {k}."
    );
    ensure!(
        parity_shards >= 1 && k + parity_shards <= MAX_TOTAL_SHARDS,
        "Invalid RS parity shard count: k={k}, m={parity_shards}."
    );
    let shard_len = data_shards[0].len();
    ensure!(shard_len > 0, "RS shard length must be positive.");
    for shard in data_shards {
        ensure!(shard.len() == shard_len, "RS data shard length mismatch.");
    }

    let mut parity = vec![vec![0u8; shard_len]; parity_shards];
    for (p_idx, p_buf) in parity.iter_mut().enumerate() {
        for (d_idx, d_buf) in data_shards.iter().enumerate() {
            let coeff = cauchy_coeff(k, p_idx, d_idx);
            gf_mul_slice_xor(p_buf, d_buf, coeff);
        }
    }
    Ok(parity)
}

/// Reconstructs all `k` data shards from any `>= k` received shards (indexed `0..k+m`).
///
/// Fast-paths when all `k` data shards are present (zero GF arithmetic). Otherwise,
/// solves only the `s x s` Cauchy sub-system for the `s <= m` missing data shards.
pub fn reconstruct_data_shards(
    data_shards: usize,
    parity_shards: usize,
    shard_len: usize,
    shards: &mut [Option<Vec<u8>>],
) -> Result<()> {
    let k = data_shards;
    let m = parity_shards;
    ensure!(
        (1..=MAX_DATA_SHARDS).contains(&k) && m >= 1 && k + m <= MAX_TOTAL_SHARDS,
        "Invalid RS parameters: k={k}, m={m}."
    );
    ensure!(
        shards.len() == k + m,
        "Shard buffer length ({}) does not match k+m ({}).",
        shards.len(),
        k + m
    );

    let missing_data: Vec<usize> = (0..k).filter(|&i| shards[i].is_none()).collect();
    if missing_data.is_empty() {
        return Ok(());
    }

    let needed = missing_data.len();
    let present_data: Vec<usize> = (0..k).filter(|&i| shards[i].is_some()).collect();
    let present_parity: Vec<usize> = (0..m)
        .filter(|&p| shards[k + p].is_some())
        .take(needed)
        .collect();

    ensure!(
        present_parity.len() == needed,
        "Not enough RS shards to recover: need {needed} parity shards, have {}.",
        present_parity.len()
    );

    // Build residual right-hand side for each chosen parity shard:
    //   rhs[a] = parity[q_a] ^ sum_{j in present_data} P[q_a][j] * data[j]
    //          = sum_{j in missing_data} P[q_a][j] * data[j]
    let mut rhs = Vec::with_capacity(needed);
    for &p_idx in &present_parity {
        let mut residual = shards[k + p_idx]
            .as_ref()
            .expect("selected parity shard present")
            .clone();
        ensure!(
            residual.len() == shard_len,
            "Parity shard length mismatch during RS decode."
        );
        for &d_idx in &present_data {
            let data = shards[d_idx].as_ref().expect("selected data shard present");
            ensure!(
                data.len() == shard_len,
                "Data shard length mismatch during RS decode."
            );
            let coeff = cauchy_coeff(k, p_idx, d_idx);
            gf_mul_slice_xor(&mut residual, data, coeff);
        }
        rhs.push(residual);
    }

    // Build and invert the s x s Cauchy submatrix A[a][b] = P[present_parity[a]][missing_data[b]].
    let mut submatrix = vec![vec![0u8; needed]; needed];
    for (a, &p_idx) in present_parity.iter().enumerate() {
        for (b, &d_idx) in missing_data.iter().enumerate() {
            submatrix[a][b] = cauchy_coeff(k, p_idx, d_idx);
        }
    }
    let inv = invert_matrix(submatrix)?;

    // Recover each missing data shard: data[L[b]] = sum_a inv[b][a] * rhs[a].
    for (b, &d_idx) in missing_data.iter().enumerate() {
        let mut recovered = vec![0u8; shard_len];
        for (a, rhs_row) in rhs.iter().enumerate() {
            gf_mul_slice_xor(&mut recovered, rhs_row, inv[b][a]);
        }
        shards[d_idx] = Some(recovered);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Frame-level QUIC datagram encoding & viewer reassembly
// ---------------------------------------------------------------------------

/// Metadata + payload of a recovered or transmitted video frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FecFrame {
    pub sequence: u64,
    pub timestamp: u64,
    pub host_latency_us: u32,
    pub bytes: Vec<u8>,
}

/// Encodes a video frame into `k + m` self-describing QUIC datagrams (each prefixed with
/// [`FEC_VIDEO_MAGIC`]).
///
/// Returns `Ok(None)` if the frame does not fit within FEC datagram limits for
/// `max_datagram_size` (caller should fall back to a unidirectional QUIC stream).
pub fn encode_frame_datagrams(
    sequence: u64,
    timestamp: u64,
    host_latency_us: u32,
    payload: &[u8],
    max_datagram_size: usize,
) -> Result<Option<Vec<Bytes>>> {
    if !can_encode_frame(payload.len(), max_datagram_size) {
        return Ok(None);
    }
    let frame_len = u32::try_from(payload.len())?;
    let max_payload = max_datagram_size - FEC_HEADER_SIZE;
    let k = payload.len().div_ceil(max_payload);
    let m = parity_shard_count(k);
    let shard_size = payload.len().div_ceil(k);

    // Prepare k equal-sized data shards (only the trailing partial/empty shard needs padding).
    let full_shards = payload.len() / shard_size;
    let partial_start = full_shards * shard_size;
    let mut padded_partial: Vec<u8> = Vec::new();
    if partial_start < payload.len() {
        padded_partial.reserve(shard_size);
        padded_partial.extend_from_slice(&payload[partial_start..]);
        padded_partial.resize(shard_size, 0);
    }
    let zero_shard: Vec<u8> = if full_shards + usize::from(!padded_partial.is_empty()) < k {
        vec![0u8; shard_size]
    } else {
        Vec::new()
    };

    let mut data_slices: Vec<&[u8]> = Vec::with_capacity(k);
    for idx in 0..k {
        let slice = if idx < full_shards {
            let start = idx * shard_size;
            &payload[start..start + shard_size]
        } else if idx == full_shards && !padded_partial.is_empty() {
            &padded_partial[..]
        } else {
            &zero_shard[..]
        };
        data_slices.push(slice);
    }

    let parity = encode_parity_shards(&data_slices, m)?;

    let write_header = |buf: &mut BytesMut, shard_idx: usize| {
        buf.put_u8(FEC_VIDEO_MAGIC);
        buf.put_u64(sequence);
        buf.put_u64(timestamp);
        buf.put_u32(host_latency_us);
        buf.put_u32(frame_len);
        buf.put_u8(k as u8);
        buf.put_u8(m as u8);
        buf.put_u8(shard_idx as u8);
    };

    let mut packets = Vec::with_capacity(k + m);
    for (idx, slice) in data_slices.iter().enumerate() {
        let mut buf = BytesMut::with_capacity(FEC_HEADER_SIZE + shard_size);
        write_header(&mut buf, idx);
        buf.put_slice(slice);
        packets.push(buf.freeze());
    }
    for (p_idx, p_bytes) in parity.iter().enumerate() {
        let mut buf = BytesMut::with_capacity(FEC_HEADER_SIZE + shard_size);
        write_header(&mut buf, k + p_idx);
        buf.put_slice(p_bytes);
        packets.push(buf.freeze());
    }

    Ok(Some(packets))
}

#[derive(Debug)]
struct IncompleteFrame {
    timestamp: u64,
    host_latency_us: u32,
    frame_len: usize,
    data_shards: usize,
    parity_shards: usize,
    shard_size: usize,
    first_seen: Instant,
    received_count: usize,
    shards: Vec<Option<Vec<u8>>>,
}

/// Outcome of feeding one incoming datagram into [`FecReassembler`].
#[derive(Debug, Eq, PartialEq)]
pub enum PushOutcome {
    /// Datagram was not a valid/applicable FEC video shard (or was a duplicate/stale shard).
    Ignored,
    /// Shard buffered; more shards are still needed to reconstruct this frame.
    Buffered,
    /// Frame was completely reassembled (possibly via RS erasure recovery in 0-RTT).
    Completed(FecFrame),
}

/// Stateful viewer-side reassembler for FEC-protected video datagrams.
#[derive(Debug)]
pub struct FecReassembler {
    incomplete: BTreeMap<u64, IncompleteFrame>,
    completed: BTreeSet<u64>,
    timeout: Duration,
}

impl Default for FecReassembler {
    fn default() -> Self {
        Self::new(FEC_FRAME_TIMEOUT)
    }
}

impl FecReassembler {
    pub fn new(timeout: Duration) -> Self {
        Self {
            incomplete: BTreeMap::new(),
            completed: BTreeSet::new(),
            timeout,
        }
    }

    /// Marks `sequence` as already delivered/assembled (e.g. arrived via uni-stream) so
    /// any late shards for it are ignored.
    pub fn mark_completed(&mut self, sequence: u64) {
        self.incomplete.remove(&sequence);
        self.record_completed(sequence);
    }

    /// Advances the delivered watermark `previous`, discarding any stale incomplete or
    /// completed tracking state for sequences `<= previous`.
    pub fn advance_delivered(&mut self, previous: u64) {
        self.incomplete.retain(|&seq, _| seq > previous);
        self.completed.retain(|&seq| seq > previous);
    }

    /// Earliest instant at which an incomplete buffered frame will expire.
    pub fn next_expiry(&self) -> Option<Instant> {
        self.incomplete
            .values()
            .map(|frame| frame.first_seen + self.timeout)
            .min()
    }

    /// Expires incomplete frames older than `self.timeout`, returning how many
    /// genuinely unrecoverable frames were dropped.
    pub fn expire(&mut self, now: Instant) -> usize {
        let mut dropped = 0usize;
        self.incomplete.retain(|_, frame| {
            let keep = now.saturating_duration_since(frame.first_seen) < self.timeout;
            if !keep {
                dropped += 1;
            }
            keep
        });
        dropped
    }

    /// Processes a single incoming QUIC datagram.
    ///
    /// Returns `(PushOutcome, evicted_unrecoverable)` where `evicted_unrecoverable` is true
    /// if the incomplete-frame cap forced eviction of an older unfinished frame.
    pub fn push_datagram(
        &mut self,
        datagram: &[u8],
        now: Instant,
        delivered_up_to: Option<u64>,
    ) -> Result<(PushOutcome, bool)> {
        if datagram.len() <= FEC_HEADER_SIZE || datagram[0] != FEC_VIDEO_MAGIC {
            return Ok((PushOutcome::Ignored, false));
        }

        let sequence = u64::from_be_bytes(datagram[1..9].try_into()?);
        if delivered_up_to.is_some_and(|prev| sequence <= prev)
            || self.completed.contains(&sequence)
        {
            return Ok((PushOutcome::Ignored, false));
        }

        let timestamp = u64::from_be_bytes(datagram[9..17].try_into()?);
        let host_latency_us = u32::from_be_bytes(datagram[17..21].try_into()?);
        let frame_len = u32::from_be_bytes(datagram[21..25].try_into()?) as usize;
        let data_shards = usize::from(datagram[25]);
        let parity_shards = usize::from(datagram[26]);
        let shard_idx = usize::from(datagram[27]);

        if frame_len == 0
            || frame_len > MAX_ACCESS_UNIT
            || !(1..=MAX_DATA_SHARDS).contains(&data_shards)
            || parity_shards == 0
            || data_shards + parity_shards > MAX_TOTAL_SHARDS
            || shard_idx >= data_shards + parity_shards
        {
            return Ok((PushOutcome::Ignored, false));
        }

        let shard_size = frame_len.div_ceil(data_shards);
        let payload = &datagram[FEC_HEADER_SIZE..];
        if payload.len() != shard_size {
            return Ok((PushOutcome::Ignored, false));
        }

        let mut evicted_unrecoverable = false;
        if !self.incomplete.contains_key(&sequence) {
            if self.incomplete.len() >= MAX_INCOMPLETE_FRAMES {
                if let Some((&oldest_seq, _)) = self.incomplete.first_key_value() {
                    if sequence < oldest_seq {
                        return Ok((PushOutcome::Ignored, false));
                    }
                    self.incomplete.pop_first();
                    evicted_unrecoverable = true;
                }
            }
            self.incomplete.insert(
                sequence,
                IncompleteFrame {
                    timestamp,
                    host_latency_us,
                    frame_len,
                    data_shards,
                    parity_shards,
                    shard_size,
                    first_seen: now,
                    received_count: 0,
                    shards: vec![None; data_shards + parity_shards],
                },
            );
        }

        let entry = self
            .incomplete
            .get_mut(&sequence)
            .expect("incomplete frame inserted");
        if entry.timestamp != timestamp
            || entry.host_latency_us != host_latency_us
            || entry.frame_len != frame_len
            || entry.data_shards != data_shards
            || entry.parity_shards != parity_shards
            || entry.shard_size != shard_size
        {
            return Ok((PushOutcome::Ignored, evicted_unrecoverable));
        }

        if entry.shards[shard_idx].is_some() {
            return Ok((PushOutcome::Ignored, evicted_unrecoverable));
        }

        entry.shards[shard_idx] = Some(payload.to_vec());
        entry.received_count += 1;

        if entry.received_count < entry.data_shards {
            return Ok((PushOutcome::Buffered, evicted_unrecoverable));
        }

        // We have at least k shards: reconstruct in 0-RTT!
        let mut frame = self
            .incomplete
            .remove(&sequence)
            .expect("incomplete frame present");
        if let Err(err) = reconstruct_data_shards(
            frame.data_shards,
            frame.parity_shards,
            frame.shard_size,
            &mut frame.shards,
        ) {
            bail!("Failed to RS-decode frame {sequence}: {err}");
        }

        let mut out = Vec::with_capacity(frame.frame_len);
        for slot in frame.shards.into_iter().take(frame.data_shards) {
            let shard = slot.expect("all data shards present after RS decode");
            let remaining = frame.frame_len - out.len();
            let take = remaining.min(shard.len());
            out.extend_from_slice(&shard[..take]);
        }

        self.record_completed(sequence);
        Ok((
            PushOutcome::Completed(FecFrame {
                sequence,
                timestamp: frame.timestamp,
                host_latency_us: frame.host_latency_us,
                bytes: out,
            }),
            evicted_unrecoverable,
        ))
    }

    fn record_completed(&mut self, sequence: u64) {
        self.completed.insert(sequence);
        while self.completed.len() > MAX_COMPLETED_SEQUENCES {
            self.completed.pop_first();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_payload(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| {
                let x = (i as u32)
                    .wrapping_mul(2654435761)
                    .wrapping_add(u32::from(seed) * 97);
                (x >> 16) as u8
            })
            .collect()
    }

    #[test]
    fn gf256_field_axioms_and_cauchy_invertibility() {
        // Verify multiplicative inverse for all non-zero elements in GF(2^8).
        for a in 1..=255u8 {
            let inv = gf_inv(a);
            assert_eq!(gf_mul(a, inv), 1, "inverse failed for {a}");
            assert_eq!(mul_table()[usize::from(a)][usize::from(inv)], 1);
        }
        // Verify Cauchy submatrix invertibility at maximum parameters (k=180, m=36).
        let k = MAX_DATA_SHARDS;
        let m = parity_shard_count(k);
        assert_eq!(m, 36);
        let mut mat = vec![vec![0u8; m]; m];
        for i in 0..m {
            for j in 0..m {
                mat[i][j] = cauchy_coeff(k, i, j * 5);
            }
        }
        let inv = invert_matrix(mat.clone()).unwrap();
        for i in 0..m {
            for j in 0..m {
                let mut acc = 0u8;
                for t in 0..m {
                    acc ^= gf_mul(mat[i][t], inv[t][j]);
                }
                assert_eq!(acc, u8::from(i == j));
            }
        }
    }

    #[test]
    fn fec_round_trip_zero_loss_small_and_large_frames() {
        let mtu = 1200;
        for &size in &[1usize, 6, 19, 1172, 1173, 15_000, 120_000, 190_000] {
            let payload = synthetic_payload(size, (size & 0xff) as u8);
            let packets = encode_frame_datagrams(42, 90_000, 1234, &payload, mtu)
                .unwrap()
                .expect("frame should fit in FEC datagrams");
            let k = packets[0][25] as usize;
            let m = packets[0][26] as usize;
            assert_eq!(packets.len(), k + m);
            assert!(m >= 2);
            for pkt in &packets {
                assert!(pkt.len() <= mtu);
                assert_eq!(pkt[0], FEC_VIDEO_MAGIC);
            }

            let mut reassembler = FecReassembler::default();
            let now = Instant::now();
            let mut completed = None;
            for pkt in &packets {
                let (outcome, evicted) = reassembler.push_datagram(pkt, now, None).unwrap();
                assert!(!evicted);
                if let PushOutcome::Completed(frame) = outcome {
                    assert!(completed.is_none(), "frame must complete only once");
                    completed = Some(frame);
                }
            }
            let frame = completed.expect("frame should complete");
            assert_eq!(frame.sequence, 42);
            assert_eq!(frame.timestamp, 90_000);
            assert_eq!(frame.host_latency_us, 1234);
            assert_eq!(frame.bytes, payload);
            // Trailing parity shards must not leave any phantom incomplete buckets.
            assert!(reassembler.next_expiry().is_none());
        }
    }

    #[test]
    fn fec_recovers_single_and_max_m_losses_and_out_of_order_delivery() {
        let mtu = 1200;
        let payload = synthetic_payload(45_123, 7);
        let packets = encode_frame_datagrams(10, 123_456, 777, &payload, mtu)
            .unwrap()
            .unwrap();
        let k = packets[0][25] as usize;
        let m = packets[0][26] as usize;
        assert!(k >= 35 && m >= 7);

        // 1. Single data shard loss (shard 0 missing).
        {
            let mut reassembler = FecReassembler::default();
            let now = Instant::now();
            let mut completed = None;
            for (idx, pkt) in packets.iter().enumerate() {
                if idx == 0 {
                    continue;
                }
                if let (PushOutcome::Completed(frame), _) =
                    reassembler.push_datagram(pkt, now, None).unwrap()
                {
                    completed = Some(frame);
                }
            }
            assert_eq!(completed.unwrap().bytes, payload);
        }

        // 2. Maximum m data shards lost (first m data shards missing, only remaining k arrive),
        //    delivered in reverse/out-of-order!
        {
            let mut reassembler = FecReassembler::default();
            let now = Instant::now();
            let mut remaining: Vec<&Bytes> = packets[m..].iter().collect();
            remaining.reverse();
            assert_eq!(remaining.len(), k);
            let mut completed = None;
            for pkt in remaining {
                if let (PushOutcome::Completed(frame), _) =
                    reassembler.push_datagram(pkt, now, None).unwrap()
                {
                    completed = Some(frame);
                }
            }
            let frame = completed.expect("must recover with exactly k shards after m losses");
            assert_eq!(frame.bytes, payload);
        }

        // 3. Mixed data + parity losses totaling m, out-of-order.
        {
            let mut reassembler = FecReassembler::default();
            let now = Instant::now();
            let drop_indices: BTreeSet<usize> = (0..m)
                .map(|i| if i % 2 == 0 { i * 3 } else { k + (i / 2) })
                .collect();
            assert_eq!(drop_indices.len(), m);
            let mut kept: Vec<&Bytes> = packets
                .iter()
                .enumerate()
                .filter(|(i, _)| !drop_indices.contains(i))
                .map(|(_, p)| p)
                .collect();
            // Interleave front/back for deterministic out-of-order arrival.
            kept.sort_by_key(|p| (p[27].wrapping_mul(17)) % 251);
            let mut completed = None;
            for pkt in kept {
                if let (PushOutcome::Completed(frame), _) =
                    reassembler.push_datagram(pkt, now, None).unwrap()
                {
                    completed = Some(frame);
                }
            }
            assert_eq!(completed.unwrap().bytes, payload);
        }

        // 4. Tiny 1-shard frame surviving loss of its ONLY data shard + 1 parity shard!
        {
            let tiny = b"tiny1".to_vec();
            let tiny_pkts = encode_frame_datagrams(99, 1, 2, &tiny, mtu)
                .unwrap()
                .unwrap();
            assert_eq!(tiny_pkts[0][25], 1);
            assert_eq!(tiny_pkts[0][26], 2);
            let mut reassembler = FecReassembler::default();
            // Drop data shard 0 AND parity shard 1; recover from parity shard 2 alone!
            let (outcome, _) = reassembler
                .push_datagram(&tiny_pkts[2], Instant::now(), None)
                .unwrap();
            assert_eq!(
                outcome,
                PushOutcome::Completed(FecFrame {
                    sequence: 99,
                    timestamp: 1,
                    host_latency_us: 2,
                    bytes: tiny,
                })
            );
        }
    }

    #[test]
    fn fec_expires_unrecoverable_frame_and_rejects_oversized_for_fallback() {
        let mtu = 1200;
        // Oversized 300 KiB frame returns Ok(None) so caller falls back to uni-stream.
        let oversized = synthetic_payload(300 * 1024, 3);
        assert!(
            encode_frame_datagrams(1, 0, 0, &oversized, mtu)
                .unwrap()
                .is_none()
        );

        // Unrecoverable frame (m + 1 shards lost) stays incomplete and expires after timeout.
        let payload = synthetic_payload(12_000, 9);
        let packets = encode_frame_datagrams(5, 500, 10, &payload, mtu)
            .unwrap()
            .unwrap();
        let k = packets[0][25] as usize;
        let mut reassembler = FecReassembler::new(Duration::from_millis(50));
        let t0 = Instant::now();
        for pkt in packets.iter().take(k - 1) {
            let (outcome, _) = reassembler.push_datagram(pkt, t0, None).unwrap();
            assert_eq!(outcome, PushOutcome::Buffered);
        }
        assert_eq!(reassembler.expire(t0 + Duration::from_millis(30)), 0);
        assert_eq!(reassembler.expire(t0 + Duration::from_millis(60)), 1);
        assert!(reassembler.next_expiry().is_none());
    }
}
