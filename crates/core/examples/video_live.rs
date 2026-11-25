//! Connects a live hardware encoder and decoder to the video transport.
//! The encoder command writes HEVC Annex B with AUDs to stdout. The decoder
//! command reads HEVC from stdin. The commands need not run in the same process.
use std::{
    env,
    io::{BufRead, BufWriter, Write, stdin},
    net::SocketAddrV4,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use remoters_core::{
    network::Pairing,
    session::webrtc::{SessionHost, ViewerSession},
    video::{AnnexBReader, Codec, Format},
};
use tokio::{
    sync::mpsc,
    time::{sleep_until, timeout},
};

const FORMAT: Format = Format {
    codec: Codec::Hevc,
    width: 3840,
    height: 2160,
    frames_per_second: 120,
};

fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [mode, address, separator, program, arguments @ ..] = args.as_slice() else {
        bail!("Usage: video_live send|recv IP:PORT -- CODEC_PROGRAM ARGS...");
    };
    ensure!(separator == "--", "Put -- before the codec command.");
    let address = address.parse()?;
    let mut command = Command::new(program);
    command.args(arguments);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            match mode.as_str() {
                "send" => send(address, command).await,
                "recv" => receive(address, command).await,
                _ => bail!("Expected send or recv."),
            }
        })
}

async fn send(address: SocketAddrV4, mut command: Command) -> Result<()> {
    let host = SessionHost::bind(address, FORMAT, true)?;
    println!("{} {}", host.address()?, host.pairing_code());
    std::io::stdout().flush()?;
    let mut sender = timeout(Duration::from_secs(60), host.accept()).await??;
    let mut child = CodecProcess(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let stdout = child.0.stdout.take().context("Missing encoder stdout.")?;
    let (frames, mut encoded) = mpsc::channel(2);
    let reader = thread::spawn(move || -> Result<()> {
        let mut source = AnnexBReader::new(stdout, FORMAT.codec);
        while let Some(unit) = source.next_unit()? {
            if frames.blocking_send(unit).is_err() {
                break;
            }
        }
        Ok(())
    });

    let mut first = None;
    let mut sent = 0;
    let mut dropped = 0;
    while let Some(unit) = encoded.recv().await {
        let started = *first.get_or_insert_with(Instant::now);
        let offset =
            Duration::from_secs_f64(unit.sequence as f64 / f64::from(FORMAT.frames_per_second));
        sleep_until((started + offset).into()).await;
        if sender.send(&unit).await? {
            sent += 1;
        } else {
            dropped += 1;
        }
    }
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("Encoder output thread failed."))??;
    ensure!(child.0.wait()?.success(), "Encoder process failed.");
    ensure!(sent > 0, "Encoder produced no complete pictures.");
    report(
        "Queued for WebRTC",
        sent,
        dropped,
        first.context("No video frames.")?.elapsed(),
    );
    sender.finish().await
}

async fn receive(address: SocketAddrV4, mut command: Command) -> Result<()> {
    let mut line = String::new();
    stdin().lock().read_line(&mut line)?;
    let mut receiver = ViewerSession::connect(address, Pairing::parse(&line)?).await?;
    let mut child = CodecProcess(command.stdin(Stdio::piped()).spawn()?);
    let pipe = child.0.stdin.take().context("Missing decoder stdin.")?;
    let (frames, mut to_decode) = mpsc::channel::<remoters_core::video::ReceivedFrame>(8);
    let writer = thread::spawn(move || -> Result<()> {
        let mut decoder = BufWriter::new(pipe);
        while let Some(unit) = to_decode.blocking_recv() {
            decoder.write_all(&unit.bytes)?;
            decoder.flush()?;
        }
        Ok(())
    });

    let mut first = None;
    let mut duration = Duration::ZERO;
    let mut delivered = 0;
    let mut previous = None::<u64>;
    let ticks = 90_000 / u64::from(FORMAT.frames_per_second);
    let mut lost = 0;
    loop {
        let idle = if delivered == 0 {
            Duration::from_secs(7)
        } else {
            Duration::from_secs(1)
        };
        match timeout(idle, receiver.recv()).await {
            Ok(Ok(Some(unit))) => {
                let started = *first.get_or_insert_with(Instant::now);
                ensure!(unit.codec == FORMAT.codec, "Unexpected video codec.");
                if let Some(previous) = previous {
                    let span = unit.rtp_timestamp.saturating_sub(previous);
                    lost += ((span + ticks / 2) / ticks).saturating_sub(1);
                }
                previous = Some(unit.rtp_timestamp);
                frames
                    .send(unit)
                    .await
                    .context("Decoder stopped reading.")?;
                delivered += 1;
                duration = started.elapsed();
            }
            Ok(Ok(None)) => break,
            Ok(Err(error)) => return Err(error),
            Err(_) if delivered > 0 => break,
            Err(error) => return Err(error.into()),
        }
    }
    report("Delivered to decoder", delivered, lost, duration);
    drop(frames);
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("Decoder input thread failed."))??;
    ensure!(child.0.wait()?.success(), "Decoder process failed.");
    Ok(())
}

fn report(action: &str, count: u64, lost: u64, duration: Duration) {
    let elapsed = duration.as_secs_f64().max(f64::EPSILON);
    eprintln!(
        "{action}: {count} pictures, {lost} missed, {:.1} fps over {:.3}s",
        count.saturating_sub(1) as f64 / elapsed,
        elapsed
    );
}

struct CodecProcess(Child);

impl Drop for CodecProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
