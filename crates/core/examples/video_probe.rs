//! A headless benchmark for the compressed-video transport. Input must be
//! HEVC Annex B with access-unit delimiters, e.g. an FFmpeg hardware encode
//! with `-bsf:v hevc_metadata=aud=insert -f hevc`.

use std::{
    env, fs,
    io::{BufRead, BufWriter, Write, stdin},
    net::SocketAddrV4,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use remoters_core::{
    network::Pairing,
    session::webrtc::{SessionHost, ViewerSession},
    video::{AccessUnit, Codec, Format, access_units},
};
use tokio::time::{Instant, sleep_until, timeout};

const FPS: u16 = 120;
const FORMAT: Format = Format {
    codec: Codec::Hevc,
    width: 3840,
    height: 2160,
    frames_per_second: FPS,
    hdr: false,
};

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
        match args.as_slice() {
            [mode, address, path, bitrate] if mode == "host" => {
                let units = read_units(path, bitrate.parse()?)?;
                let host = SessionHost::bind(address.parse()?, FORMAT, true)?;
                // This is a short-lived test invitation. Keep it in the terminal.
                println!("{} {}", host.address()?, host.pairing_code());
                std::io::stdout().flush()?;
                send(&host, &units, Instant::now()).await
            }
            [mode, address, path, count] if mode == "recv" => {
                let mut line = String::new();
                stdin().lock().read_line(&mut line)?;
                let mut receiver = ViewerSession::connect(address.parse()?, Pairing::parse(&line)?).await?;
                let mut output = BufWriter::new(fs::File::create(path)?);
                let count: usize = count.parse()?;
                let first = Instant::now();
                let mut base = None;
                for index in 0..count {
                    let unit = timeout(Duration::from_secs(3), receiver.recv()).await??.context("Video ended early.")?;
                    ensure!(unit.codec == FORMAT.codec, "Unexpected video codec.");
                    let origin = *base.get_or_insert(unit.rtp_timestamp);
                    ensure!(unit.rtp_timestamp - origin == index as u64 * 90_000 / u64::from(FPS), "A frame was lost before index {index}.");
                    output.write_all(&unit.bytes)?;
                }
                output.flush()?;
                println!("Received {count} complete pictures in {:.3}s.", first.elapsed().as_secs_f64());
                Ok(())
            }
            [mode, path, bitrate] if mode == "loopback" => {
                let units = read_units(path, bitrate.parse()?)?;
                let count = units.len();
                let host = SessionHost::bind("127.0.0.1:0".parse::<SocketAddrV4>()?, FORMAT, true)?;
                let address = host.address()?;
                let pairing = Pairing::parse(&host.pairing_code())?;
                let start = Instant::now() + Duration::from_millis(400);
                let server = tokio::spawn(async move { send(&host, &units, start).await });
                let mut receiver = ViewerSession::connect(address, pairing).await?;
                let mut timings = Vec::with_capacity(count);
                let mut base = None;
                for index in 0..count {
                    let unit = timeout(Duration::from_secs(3), receiver.recv()).await??.context("Video ended early.")?;
                    ensure!(unit.codec == FORMAT.codec, "Unexpected video codec.");
                    let origin = *base.get_or_insert(unit.rtp_timestamp);
                    ensure!(unit.rtp_timestamp - origin == index as u64 * 90_000 / u64::from(FPS), "A frame was lost before index {index}.");
                    let scheduled = start + frame_offset(index);
                    timings.push(Instant::now().saturating_duration_since(scheduled).as_secs_f64() * 1000.);
                }
                let rate = (count - 1) as f64 / (Instant::now() - start).as_secs_f64();
                timings.sort_by(f64::total_cmp);
                println!("{count} pictures, {rate:.1} fps; scheduling and transport p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms",
                    timings[count / 2], timings[count * 95 / 100], timings[count * 99 / 100]);
                server.await?
            }
            _ => bail!("Usage: video_probe host IP:PORT INPUT.hevc MBPS | recv IP:PORT OUTPUT.hevc COUNT (code on stdin) | loopback INPUT.hevc MBPS"),
        }
    })
}

fn read_units(path: &str, mbps: usize) -> Result<Vec<AccessUnit>> {
    ensure!(mbps <= 200, "The probe supports padding up to 200 Mbps.");
    let recording = Bytes::from(fs::read(path)?);
    let target = mbps * 1_000_000 / 8 / usize::from(FPS);
    let units: Vec<_> = access_units(Codec::Hevc, recording)
        .map(|unit| {
            let unit = unit?;
            if unit.bytes().len() + 7 >= target {
                return Ok(unit);
            }
            // HEVC filler NAL, preserving the real encoded pictures while
            // testing transport throughput at the requested nominal bitrate.
            let mut stuffed = Vec::with_capacity(target);
            stuffed.extend_from_slice(unit.bytes());
            stuffed.extend_from_slice(&[0, 0, 1, 76, 1]);
            stuffed.resize(target - 1, 0xff);
            stuffed.push(0x80);
            AccessUnit::from_annex_b(Codec::Hevc, unit.sequence, stuffed.into())
        })
        .collect::<Result<_>>()?;
    ensure!(
        units.len() >= 2 && units[0].is_random_access(),
        "The input needs delimited HEVC access units starting at an IDR with VPS/SPS/PPS."
    );
    Ok(units)
}

fn frame_offset(index: usize) -> Duration {
    Duration::from_nanos(index as u64 * 1_000_000_000 / u64::from(FPS))
}

async fn send(host: &SessionHost, units: &[AccessUnit], earliest: Instant) -> Result<()> {
    let mut sender = timeout(Duration::from_secs(60), host.accept()).await??;
    let start = earliest.max(Instant::now());
    for (index, unit) in units.iter().enumerate() {
        sleep_until(start + frame_offset(index)).await;
        ensure!(
            sender.send(unit).await?,
            "Frame {index} missed its send deadline."
        );
    }
    ensure!(
        units.last().context("No pictures.")?.sequence + 1 == units.len() as u64,
        "Unexpected sequence."
    );
    println!("Sent {} complete pictures.", units.len());
    sender.finish().await
}
