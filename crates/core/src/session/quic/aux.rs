use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use quinn::{Connection, RecvStream, SendStream};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::timeout,
};

use super::{
    aux_types::{
        AudioFrame, AudioQueueGuard, AudioSendQueue, ClipboardData, ClipboardImageFormat,
        CursorImage, CursorShape, CursorUpdate, MAX_CLIPBOARD_IMAGE_BYTES,
        MAX_CLIPBOARD_TEXT_BYTES, MAX_CURSOR_DIMENSION, MAX_FILE_TRANSFER_BYTES,
        MAX_RELATIVE_PATH_BYTES, ReceivedFile, SessionAuxiliary, sanitize_relative_path,
        unique_destination, validate_audio_params,
    },
    transport::{normal_close, normal_read_close},
};

const AUX_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const AUX_CLIPBOARD: u8 = 1;
const AUX_FILE: u8 = 2;
const AUX_CURSOR: u8 = 3;
pub(super) const AUX_AUDIO: u8 = 4;
const AUX_FILE_CHUNK_SIZE: usize = 128 * 1024;

#[derive(Clone)]
pub struct AuxSender {
    pub(super) connection: Connection,
    pub(super) cursor: Option<watch::Sender<Option<CursorUpdate>>>,
    pub(super) audio: Option<Arc<AudioSendQueue>>,
}

impl AuxSender {
    pub fn send_cursor(&self, update: CursorUpdate) -> Result<()> {
        update.validate()?;
        let cursor = self
            .cursor
            .as_ref()
            .context("Cursor updates can only be sent from the host.")?;
        cursor
            .send(Some(update))
            .map_err(|_| anyhow::anyhow!("Host cursor channel closed."))
    }

    pub fn send_audio(&self, frame: AudioFrame) -> Result<()> {
        frame.validate()?;
        let audio = self
            .audio
            .as_ref()
            .context("Audio frames can only be sent from the host.")?;
        audio.push(frame)
    }

    pub async fn send_clipboard(&self, data: &ClipboardData) -> Result<()> {
        let (variant, payload): (u8, &[u8]) = match data {
            ClipboardData::Text(text) => {
                ensure!(
                    text.len() <= MAX_CLIPBOARD_TEXT_BYTES,
                    "Clipboard text exceeds the size limit."
                );
                (0, text.as_bytes())
            }
            ClipboardData::Image { format, bytes } => {
                ensure!(
                    bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES,
                    "Clipboard image exceeds the size limit."
                );
                (*format as u8, bytes.as_slice())
            }
        };
        let (mut send, mut recv) = self.connection.open_bi().await?;
        send.set_priority(2)?;
        let mut header = [0u8; 6];
        header[0] = AUX_CLIPBOARD;
        header[1] = variant;
        header[2..6].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        send.write_all(&header).await?;
        send.write_all(payload).await?;
        send.finish()?;
        let mut ack = [0u8; 1];
        timeout(AUX_ACK_TIMEOUT, recv.read_exact(&mut ack)).await??;
        ensure!(ack[0] == 0, "Remote clipboard sync was rejected.");
        Ok(())
    }

    pub async fn send_file<F>(
        &self,
        relative_path: &str,
        source_path: &Path,
        mut on_progress: F,
    ) -> Result<u64>
    where
        F: FnMut(u64, u64),
    {
        let sanitized = sanitize_relative_path(relative_path)?;
        let wire_path = sanitized
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let path_bytes = wire_path.as_bytes();
        ensure!(
            path_bytes.len() <= MAX_RELATIVE_PATH_BYTES
                && path_bytes.len() <= usize::from(u16::MAX),
            "Relative file path is too long."
        );

        let src = source_path.to_path_buf();
        let (mut file, file_size) = tokio::task::spawn_blocking(move || -> Result<_> {
            let meta = std::fs::metadata(&src)
                .with_context(|| format!("Cannot access {}.", src.display()))?;
            ensure!(meta.is_file(), "{} is not a regular file.", src.display());
            let len = meta.len();
            ensure!(
                len <= MAX_FILE_TRANSFER_BYTES,
                "File exceeds the 2 GiB transfer limit."
            );
            let file = std::fs::File::open(&src)
                .with_context(|| format!("Cannot open {}.", src.display()))?;
            Ok((file, len))
        })
        .await??;

        let (mut send, mut recv) = self.connection.open_bi().await?;
        send.set_priority(-5)?;
        let mut header = Vec::with_capacity(11 + path_bytes.len());
        header.push(AUX_FILE);
        header.extend_from_slice(&(path_bytes.len() as u16).to_be_bytes());
        header.extend_from_slice(path_bytes);
        header.extend_from_slice(&file_size.to_be_bytes());
        send.write_all(&header).await?;

        on_progress(0, file_size);
        let mut sent = 0u64;
        let mut buf = vec![0u8; AUX_FILE_CHUNK_SIZE];
        while sent < file_size {
            let (returned_file, returned_buf, count) =
                tokio::task::spawn_blocking(move || -> Result<_> {
                    let count = file.read(&mut buf)?;
                    Ok((file, buf, count))
                })
                .await??;
            file = returned_file;
            buf = returned_buf;
            if count == 0 {
                break;
            }
            send.write_all(&buf[..count]).await?;
            sent += count as u64;
            on_progress(sent, file_size);
        }
        ensure!(sent == file_size, "Source file changed size while sending.");
        send.finish()?;
        let mut ack = [0u8; 1];
        timeout(AUX_ACK_TIMEOUT, recv.read_exact(&mut ack)).await??;
        ensure!(ack[0] == 0, "Remote host rejected the file transfer.");
        Ok(sent)
    }
}

pub(super) async fn recv_optional_aux(
    rx: &mut Option<mpsc::Receiver<SessionAuxiliary>>,
) -> Option<SessionAuxiliary> {
    match rx.as_mut() {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

pub(super) async fn cursor_send_loop(
    connection: Connection,
    mut cursor_rx: watch::Receiver<Option<CursorUpdate>>,
) -> Result<()> {
    let mut stream: Option<(SendStream, RecvStream)> = None;
    loop {
        if cursor_rx.changed().await.is_err() {
            break;
        }
        let Some(update) = cursor_rx.borrow_and_update().clone() else {
            continue;
        };
        let encoded = update.encode()?;
        let send = match stream.as_mut() {
            Some((send, _)) => send,
            None => {
                let (mut send, recv) = connection.open_bi().await?;
                send.set_priority(4)?;
                send.write_all(&[AUX_CURSOR]).await?;
                &mut stream.insert((send, recv)).0
            }
        };
        send.write_all(&encoded).await?;
    }
    if let Some((mut send, _)) = stream {
        let _ = send.finish();
    }
    Ok(())
}

async fn read_cursor_update(recv: &mut RecvStream) -> Result<Option<CursorUpdate>> {
    let mut header = [0u8; 2];
    match recv.read_exact(&mut header).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(quinn::ReadExactError::ReadError(error)) if normal_read_close(&error) => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }
    let shape = CursorShape::try_from(header[0])?;
    let has_custom = match header[1] {
        0 => false,
        1 => true,
        other => bail!("Invalid custom cursor flag: {other}."),
    };
    if !has_custom {
        return Ok(Some(CursorUpdate::new(shape)));
    }
    let mut meta = [0u8; 8];
    recv.read_exact(&mut meta).await?;
    let width = u16::from_be_bytes([meta[0], meta[1]]);
    let height = u16::from_be_bytes([meta[2], meta[3]]);
    let hotspot_x = u16::from_be_bytes([meta[4], meta[5]]);
    let hotspot_y = u16::from_be_bytes([meta[6], meta[7]]);
    ensure!(
        (1..=MAX_CURSOR_DIMENSION).contains(&width) && (1..=MAX_CURSOR_DIMENSION).contains(&height),
        "Cursor image dimensions out of range."
    );
    let rgba_len = usize::from(width) * usize::from(height) * 4;
    let mut rgba = vec![0u8; rgba_len];
    recv.read_exact(&mut rgba).await?;
    let image = CursorImage::new(width, height, hotspot_x, hotspot_y, rgba)?;
    CursorUpdate::with_custom(shape, image).map(Some)
}

pub(super) async fn audio_send_loop(
    connection: Connection,
    queue: Arc<AudioSendQueue>,
) -> Result<()> {
    let _guard = AudioQueueGuard(queue.clone());
    let mut stream: Option<(SendStream, RecvStream)> = None;
    loop {
        let Some(frame) = queue.pop() else {
            queue.notify.notified().await;
            continue;
        };
        let encoded = frame.encode()?;
        let send = match stream.as_mut() {
            Some((send, _)) => send,
            None => {
                let (mut send, recv) = connection.open_bi().await?;
                send.set_priority(5)?;
                send.write_all(&[AUX_AUDIO]).await?;
                &mut stream.insert((send, recv)).0
            }
        };
        send.write_all(&encoded).await?;
    }
}

async fn read_audio_frame(recv: &mut RecvStream) -> Result<Option<AudioFrame>> {
    let mut header = [0u8; 8];
    match recv.read_exact(&mut header).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(quinn::ReadExactError::ReadError(error)) if normal_read_close(&error) => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }
    let sample_rate = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let channels = header[4];
    let _reserved = header[5];
    let sample_count = usize::from(u16::from_be_bytes([header[6], header[7]]));
    validate_audio_params(sample_rate, channels, sample_count)?;
    let mut raw = vec![0u8; sample_count * 2];
    recv.read_exact(&mut raw).await?;
    let pcm = raw
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    Ok(Some(AudioFrame {
        sample_rate,
        channels,
        pcm,
    }))
}

pub(super) async fn accept_aux_loop(
    connection: Connection,
    allow_incoming: bool,
    allow_cursor: bool,
    receive_dir: Option<PathBuf>,
    tx: mpsc::Sender<SessionAuxiliary>,
    audio_tx: Option<mpsc::Sender<AudioFrame>>,
) -> Result<()> {
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            stream = connection.accept_bi() => match stream {
                Ok((send, recv)) => {
                    let tx = tx.clone();
                    let audio_tx = audio_tx.clone();
                    let receive_dir = receive_dir.clone();
                    tasks.spawn(async move {
                        let _ = handle_aux_stream(
                            send,
                            recv,
                            allow_incoming,
                            allow_cursor,
                            receive_dir,
                            tx,
                            audio_tx,
                        )
                        .await;
                    });
                }
                Err(error) if normal_close(&error) => return Ok(()),
                Err(_) => return Ok(()),
            },
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

async fn handle_aux_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    allow_incoming: bool,
    allow_cursor: bool,
    receive_dir: Option<PathBuf>,
    tx: mpsc::Sender<SessionAuxiliary>,
    audio_tx: Option<mpsc::Sender<AudioFrame>>,
) -> Result<()> {
    let mut kind = [0u8; 1];
    recv.read_exact(&mut kind).await?;
    if kind[0] == AUX_CURSOR {
        if !allow_cursor {
            let _ = send.write_all(&[1u8]).await;
            let _ = send.finish();
            bail!("Incoming cursor updates are viewer-only.");
        }
        while let Some(update) = read_cursor_update(&mut recv).await? {
            tx.send(SessionAuxiliary::Cursor(update))
                .await
                .context("Auxiliary receiver closed.")?;
        }
        let _ = send.finish();
        return Ok(());
    }
    if kind[0] == AUX_AUDIO {
        let Some(audio_tx) = audio_tx else {
            let _ = send.write_all(&[1u8]).await;
            let _ = send.finish();
            bail!("Incoming audio streams are viewer-only.");
        };
        while let Some(frame) = read_audio_frame(&mut recv).await? {
            let _ = audio_tx.try_send(frame);
        }
        let _ = send.finish();
        return Ok(());
    }
    let outcome: Result<()> = match kind[0] {
        AUX_CLIPBOARD => {
            if !allow_incoming {
                Err(anyhow::anyhow!("Incoming clipboard disabled."))
            } else {
                let mut meta = [0u8; 5];
                recv.read_exact(&mut meta).await?;
                let variant = meta[0];
                let len = u32::from_be_bytes([meta[1], meta[2], meta[3], meta[4]]) as usize;
                let data = if variant == 0 {
                    ensure!(
                        len <= MAX_CLIPBOARD_TEXT_BYTES,
                        "Clipboard text exceeds limit."
                    );
                    let mut bytes = vec![0u8; len];
                    recv.read_exact(&mut bytes).await?;
                    ClipboardData::Text(
                        String::from_utf8(bytes).context("Invalid UTF-8 clipboard text.")?,
                    )
                } else {
                    let format = ClipboardImageFormat::from_byte(variant)
                        .context("Unsupported clipboard image format.")?;
                    ensure!(
                        len <= MAX_CLIPBOARD_IMAGE_BYTES,
                        "Clipboard image exceeds limit."
                    );
                    let mut bytes = vec![0u8; len];
                    recv.read_exact(&mut bytes).await?;
                    ClipboardData::Image { format, bytes }
                };
                tx.send(SessionAuxiliary::Clipboard(data))
                    .await
                    .context("Auxiliary receiver closed.")?;
                Ok(())
            }
        }
        AUX_FILE => {
            if !allow_incoming {
                Err(anyhow::anyhow!("Incoming files disabled."))
            } else if let Some(root) = receive_dir {
                let mut path_len_bytes = [0u8; 2];
                recv.read_exact(&mut path_len_bytes).await?;
                let path_len = usize::from(u16::from_be_bytes(path_len_bytes));
                ensure!(
                    (1..=MAX_RELATIVE_PATH_BYTES).contains(&path_len),
                    "Invalid incoming file path length."
                );
                let mut path_bytes = vec![0u8; path_len];
                recv.read_exact(&mut path_bytes).await?;
                let raw_path =
                    String::from_utf8(path_bytes).context("Invalid UTF-8 relative path.")?;
                let rel_path = sanitize_relative_path(&raw_path)?;
                let mut size_bytes = [0u8; 8];
                recv.read_exact(&mut size_bytes).await?;
                let file_size = u64::from_be_bytes(size_bytes);
                ensure!(
                    file_size <= MAX_FILE_TRANSFER_BYTES,
                    "Incoming file exceeds size limit."
                );

                let (dest_path, mut file) = tokio::task::spawn_blocking(move || -> Result<_> {
                    std::fs::create_dir_all(&root)?;
                    let dest = unique_destination(&root, &rel_path)?;
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&dest)?;
                    Ok((dest, file))
                })
                .await??;

                let stream_result: Result<()> = async {
                    let mut written = 0u64;
                    let mut buf = vec![0u8; AUX_FILE_CHUNK_SIZE];
                    loop {
                        match recv.read(&mut buf).await? {
                            Some(n) => {
                                ensure!(
                                    written.saturating_add(n as u64) <= file_size,
                                    "Received more bytes than declared."
                                );
                                let (returned_file, returned_buf) =
                                    tokio::task::spawn_blocking(move || -> Result<_> {
                                        file.write_all(&buf[..n])?;
                                        Ok((file, buf))
                                    })
                                    .await??;
                                file = returned_file;
                                buf = returned_buf;
                                written += n as u64;
                            }
                            None => break,
                        }
                    }
                    ensure!(written == file_size, "Truncated file transfer.");
                    tokio::task::spawn_blocking(move || file.flush()).await??;
                    Ok(())
                }
                .await;

                if let Err(error) = stream_result {
                    let cleanup = dest_path.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || std::fs::remove_file(cleanup)).await;
                    Err(error)
                } else {
                    let display_rel = raw_path;
                    tx.send(SessionAuxiliary::FileReceived(ReceivedFile {
                        relative_path: display_rel,
                        saved_path: dest_path,
                        bytes: file_size,
                    }))
                    .await
                    .context("Auxiliary receiver closed.")?;
                    Ok(())
                }
            } else {
                Err(anyhow::anyhow!("No destination folder configured."))
            }
        }
        _ => Err(anyhow::anyhow!("Unknown auxiliary stream type.")),
    };

    let ack = if outcome.is_ok() { [0u8] } else { [1u8] };
    let _ = send.write_all(&ack).await;
    let _ = send.finish();
    outcome
}
