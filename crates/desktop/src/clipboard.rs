use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use gpui::{App, ClipboardEntry, ClipboardItem, ImageFormat};
use remoters_core::session::{
    ClipboardData, ClipboardImageFormat, MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_TEXT_BYTES,
    MAX_FILE_TRANSFER_BYTES, sanitize_relative_path,
};

const MAX_DROP_FILES: usize = 500;

pub fn clipboard_fingerprint(data: &ClipboardData) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    match data {
        ClipboardData::Text(text) => {
            feed(&[0]);
            feed(text.as_bytes());
        }
        ClipboardData::Image { format, bytes } => {
            feed(&[*format as u8]);
            feed(bytes);
        }
    }
    hash
}

pub fn read_clipboard(cx: &App) -> Option<(u64, ClipboardData)> {
    #[cfg(target_os = "linux")]
    if let Some(data) = linux::read_system_clipboard() {
        return Some((clipboard_fingerprint(&data), data));
    }
    read_gpui_clipboard(cx)
}

fn read_gpui_clipboard(cx: &App) -> Option<(u64, ClipboardData)> {
    let item = cx.read_from_clipboard()?;
    for entry in item.entries() {
        match entry {
            ClipboardEntry::String(text) => {
                let value = text.text();
                if !value.is_empty() && value.len() <= MAX_CLIPBOARD_TEXT_BYTES {
                    let data = ClipboardData::Text(value.to_owned());
                    return Some((clipboard_fingerprint(&data), data));
                }
            }
            ClipboardEntry::Image(image) => {
                if !image.bytes.is_empty() && image.bytes.len() <= MAX_CLIPBOARD_IMAGE_BYTES {
                    let data = ClipboardData::Image {
                        format: from_gpui_image_format(image.format),
                        bytes: image.bytes.clone(),
                    };
                    return Some((clipboard_fingerprint(&data), data));
                }
            }
        }
    }
    None
}

pub fn write_clipboard(cx: &mut App, data: ClipboardData) -> u64 {
    let hash = clipboard_fingerprint(&data);
    #[cfg(target_os = "linux")]
    if linux::write_system_clipboard(&data) {
        return hash;
    }
    let item = match data {
        ClipboardData::Text(text) => ClipboardItem::new_string(text),
        ClipboardData::Image { format, bytes } => {
            let image = gpui::Image::from_bytes(to_gpui_image_format(format), bytes);
            ClipboardItem::new_image(&image)
        }
    };
    cx.write_to_clipboard(item);
    hash
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClipboardTarget {
    PngImage,
    Text,
}

#[cfg(any(target_os = "linux", test))]
fn clipboard_image_mime(format: ClipboardImageFormat) -> &'static str {
    match format {
        ClipboardImageFormat::Png => "image/png",
        ClipboardImageFormat::Jpeg => "image/jpeg",
        ClipboardImageFormat::Gif => "image/gif",
        ClipboardImageFormat::Webp => "image/webp",
        ClipboardImageFormat::Bmp => "image/bmp",
        ClipboardImageFormat::Svg => "image/svg+xml",
        ClipboardImageFormat::Tiff => "image/tiff",
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_clipboard_targets(targets: &str) -> Option<ClipboardTarget> {
    let mut has_text = false;
    for line in targets.lines().map(str::trim) {
        if line.eq_ignore_ascii_case("image/png") {
            return Some(ClipboardTarget::PngImage);
        }
        if matches!(
            line,
            "text/plain" | "text/plain;charset=utf-8" | "UTF8_STRING" | "STRING"
        ) {
            has_text = true;
        }
    }
    has_text.then_some(ClipboardTarget::Text)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    use remoters_core::session::{
        ClipboardData, ClipboardImageFormat, MAX_CLIPBOARD_IMAGE_BYTES, MAX_CLIPBOARD_TEXT_BYTES,
    };

    use super::{ClipboardTarget, clipboard_image_mime, parse_clipboard_targets};

    fn is_wayland() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
    }

    fn has_x11() -> bool {
        std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
    }

    fn run_read(program: &str, args: &[&str], limit: usize) -> Option<Vec<u8>> {
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() || output.stdout.is_empty() || output.stdout.len() > limit {
            return None;
        }
        Some(output.stdout)
    }

    fn run_write(program: &str, args: &[&str], bytes: &[u8]) -> bool {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(bytes).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            drop(stdin);
        }
        child.wait().is_ok_and(|status| status.success())
    }

    pub fn read_system_clipboard() -> Option<ClipboardData> {
        if is_wayland() {
            if let Some(targets_bytes) =
                run_read("wl-paste", &["--list-types"], MAX_CLIPBOARD_TEXT_BYTES)
            {
                let targets = String::from_utf8_lossy(&targets_bytes);
                match parse_clipboard_targets(&targets) {
                    Some(ClipboardTarget::PngImage) => {
                        if let Some(bytes) = run_read(
                            "wl-paste",
                            &["--type", "image/png"],
                            MAX_CLIPBOARD_IMAGE_BYTES,
                        ) {
                            return Some(ClipboardData::Image {
                                format: ClipboardImageFormat::Png,
                                bytes,
                            });
                        }
                    }
                    Some(ClipboardTarget::Text) => {
                        if let Some(bytes) = run_read(
                            "wl-paste",
                            &["--no-newline", "--type", "text/plain;charset=utf-8"],
                            MAX_CLIPBOARD_TEXT_BYTES,
                        )
                        .or_else(|| {
                            run_read("wl-paste", &["--no-newline"], MAX_CLIPBOARD_TEXT_BYTES)
                        }) && let Ok(text) = String::from_utf8(bytes)
                            && !text.is_empty()
                        {
                            return Some(ClipboardData::Text(text));
                        }
                    }
                    None => {}
                }
            }
        }
        if has_x11() {
            if let Some(targets_bytes) = run_read(
                "xclip",
                &["-o", "-selection", "clipboard", "-t", "TARGETS"],
                MAX_CLIPBOARD_TEXT_BYTES,
            ) {
                let targets = String::from_utf8_lossy(&targets_bytes);
                if parse_clipboard_targets(&targets) == Some(ClipboardTarget::PngImage)
                    && let Some(bytes) = run_read(
                        "xclip",
                        &["-o", "-selection", "clipboard", "-t", "image/png"],
                        MAX_CLIPBOARD_IMAGE_BYTES,
                    )
                {
                    return Some(ClipboardData::Image {
                        format: ClipboardImageFormat::Png,
                        bytes,
                    });
                }
            }
            if let Some(bytes) = run_read(
                "xclip",
                &["-o", "-selection", "clipboard"],
                MAX_CLIPBOARD_TEXT_BYTES,
            )
            .or_else(|| run_read("xsel", &["-ob"], MAX_CLIPBOARD_TEXT_BYTES))
                && let Ok(text) = String::from_utf8(bytes)
                && !text.is_empty()
            {
                return Some(ClipboardData::Text(text));
            }
        }
        None
    }

    pub fn write_system_clipboard(data: &ClipboardData) -> bool {
        let (mime, bytes): (&str, &[u8]) = match data {
            ClipboardData::Text(text) => ("text/plain;charset=utf-8", text.as_bytes()),
            ClipboardData::Image { format, bytes } => (clipboard_image_mime(*format), bytes),
        };
        if is_wayland() {
            let ok = match data {
                ClipboardData::Text(_) => run_write("wl-copy", &[], bytes),
                ClipboardData::Image { .. } => run_write("wl-copy", &["--type", mime], bytes),
            };
            if ok {
                return true;
            }
        }
        if has_x11() {
            let xclip_args: &[&str] = match data {
                ClipboardData::Text(_) => &["-i", "-selection", "clipboard", "-t", "UTF8_STRING"],
                ClipboardData::Image { .. } => &["-i", "-selection", "clipboard", "-t", mime],
            };
            if run_write("xclip", xclip_args, bytes) {
                return true;
            }
            if matches!(data, ClipboardData::Text(_)) && run_write("xsel", &["-ib"], bytes) {
                return true;
            }
        }
        false
    }
}

pub fn remote_receive_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME").map(PathBuf::from);

    if let Some(home) = home {
        let downloads = home.join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        let desktop = home.join("Desktop");
        if desktop.is_dir() {
            return desktop;
        }
    }
    std::env::temp_dir().join("Remoters")
}

pub fn collect_drop_entries(paths: &[PathBuf]) -> Result<Vec<(String, PathBuf)>> {
    let mut entries = Vec::new();
    for path in paths {
        let meta = std::fs::symlink_metadata(path)
            .with_context(|| format!("Cannot inspect dropped path {}.", path.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .context("Dropped item has no file name.")?
            .to_string_lossy()
            .into_owned();
        if meta.is_file() {
            ensure!(
                meta.len() <= MAX_FILE_TRANSFER_BYTES,
                "{name} exceeds the 2 GiB transfer limit."
            );
            sanitize_relative_path(&name)?;
            entries.push((name, path.clone()));
        } else if meta.is_dir() {
            collect_dir(path, &name, &mut entries)?;
        }
        ensure!(
            entries.len() <= MAX_DROP_FILES,
            "Drop at most {MAX_DROP_FILES} files at once."
        );
    }
    if entries.is_empty() {
        bail!("No regular files were found in the dropped selection.");
    }
    Ok(entries)
}

fn collect_dir(dir: &Path, prefix: &str, entries: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let mut children = std::fs::read_dir(dir)
        .with_context(|| format!("Cannot read directory {}.", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let child_path = child.path();
        let meta = std::fs::symlink_metadata(&child_path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let child_name = child.file_name().to_string_lossy().into_owned();
        let relative = format!("{prefix}/{child_name}");
        if meta.is_file() {
            ensure!(
                meta.len() <= MAX_FILE_TRANSFER_BYTES,
                "{relative} exceeds the 2 GiB transfer limit."
            );
            sanitize_relative_path(&relative)?;
            entries.push((relative, child_path));
            ensure!(
                entries.len() <= MAX_DROP_FILES,
                "Drop at most {MAX_DROP_FILES} files at once."
            );
        } else if meta.is_dir() {
            collect_dir(&child_path, &relative, entries)?;
        }
    }
    Ok(())
}

fn from_gpui_image_format(format: ImageFormat) -> ClipboardImageFormat {
    match format {
        ImageFormat::Png => ClipboardImageFormat::Png,
        ImageFormat::Jpeg => ClipboardImageFormat::Jpeg,
        ImageFormat::Gif => ClipboardImageFormat::Gif,
        ImageFormat::Webp => ClipboardImageFormat::Webp,
        ImageFormat::Bmp => ClipboardImageFormat::Bmp,
        ImageFormat::Svg => ClipboardImageFormat::Svg,
        ImageFormat::Tiff => ClipboardImageFormat::Tiff,
    }
}

fn to_gpui_image_format(format: ClipboardImageFormat) -> ImageFormat {
    match format {
        ClipboardImageFormat::Png => ImageFormat::Png,
        ClipboardImageFormat::Jpeg => ImageFormat::Jpeg,
        ClipboardImageFormat::Gif => ImageFormat::Gif,
        ClipboardImageFormat::Webp => ImageFormat::Webp,
        ClipboardImageFormat::Bmp => ImageFormat::Bmp,
        ClipboardImageFormat::Svg => ImageFormat::Svg,
        ClipboardImageFormat::Tiff => ImageFormat::Tiff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_fingerprint_distinguishes_text_and_image_formats() {
        let a = ClipboardData::Text("hello".into());
        let b = ClipboardData::Text("hello".into());
        let c = ClipboardData::Text("world".into());
        let img_png = ClipboardData::Image {
            format: ClipboardImageFormat::Png,
            bytes: vec![1, 2, 3],
        };
        let img_jpg = ClipboardData::Image {
            format: ClipboardImageFormat::Jpeg,
            bytes: vec![1, 2, 3],
        };
        assert_eq!(clipboard_fingerprint(&a), clipboard_fingerprint(&b));
        assert_ne!(clipboard_fingerprint(&a), clipboard_fingerprint(&c));
        assert_ne!(
            clipboard_fingerprint(&img_png),
            clipboard_fingerprint(&img_jpg)
        );
    }

    #[test]
    fn collect_drop_entries_handles_files_and_nested_directories() {
        let base = std::env::temp_dir().join(format!(
            "remoters-drop-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let folder = base.join("bundle");
        let nested = folder.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        let top_file = base.join("readme.md");
        let nested_file = nested.join("image.png");
        std::fs::write(&top_file, b"hi").unwrap();
        std::fs::write(&nested_file, b"png").unwrap();

        let entries = collect_drop_entries(&[top_file.clone(), folder]).unwrap();
        assert_eq!(
            entries,
            vec![
                ("readme.md".to_string(), top_file),
                ("bundle/sub/image.png".to_string(), nested_file),
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn parses_clipboard_targets_and_image_mime_types() {
        assert_eq!(
            parse_clipboard_targets("text/plain\nimage/png\nUTF8_STRING\n"),
            Some(ClipboardTarget::PngImage)
        );
        assert_eq!(
            parse_clipboard_targets("TARGETS\nUTF8_STRING\n"),
            Some(ClipboardTarget::Text)
        );
        assert_eq!(parse_clipboard_targets("application/octet-stream"), None);
        assert_eq!(clipboard_image_mime(ClipboardImageFormat::Png), "image/png");
        assert_eq!(
            clipboard_image_mime(ClipboardImageFormat::Webp),
            "image/webp"
        );
    }
}
