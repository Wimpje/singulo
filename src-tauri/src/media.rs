use crate::model::{Media, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use image::{DynamicImage, GenericImageView, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tauri::Manager;
use wait_timeout::ChildExt;

pub fn tool(app: &tauri::AppHandle, name: &str) -> Result<PathBuf> {
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    };
    let resource = app.path().resource_dir().map_err(|e| e.to_string())?;
    let bundled = resource.join("tools").join(&executable);
    if bundled.is_file() {
        return Ok(bundled);
    }
    if cfg!(debug_assertions) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        for directory in std::env::split_paths(&path).chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]) {
            let candidate = directory.join(&executable);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "{name} is unavailable. Install the development tool or use an app with bundled media tools."
    ))
}

pub fn execute(mut command: Command, timeout: u64) -> Result<Vec<u8>> {
    let output = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let errors = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    command
        .stdin(Stdio::null())
        .stdout(output.reopen().map_err(|e| e.to_string())?)
        .stderr(errors.reopen().map_err(|e| e.to_string())?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Unable to start media tool: {e}"))?;
    let status = match child
        .wait_timeout(Duration::from_secs(timeout))
        .map_err(|e| e.to_string())?
    {
        Some(status) => status,
        None => {
            child
                .kill()
                .map_err(|e| format!("Could not stop timed-out tool: {e}"))?;
            child.wait().map_err(|e| e.to_string())?;
            return Err("Media processing timed out; the original has not been changed.".into());
        }
    };
    if !status.success() {
        let mut message = String::new();
        errors
            .reopen()
            .map_err(|e| e.to_string())?
            .take(8192)
            .read_to_string(&mut message)
            .map_err(|e| e.to_string())?;
        return Err(format!("Media tool failed ({status}): {}", message.trim()));
    }
    if output
        .as_file()
        .metadata()
        .map_err(|e| e.to_string())?
        .len()
        > 256 * 1024 * 1024
    {
        return Err("Media-tool output exceeds the supported memory limit.".into());
    }
    fs::read(output.path()).map_err(|e| e.to_string())
}

pub fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 128 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub fn identity(path: &Path) -> Result<String> {
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Only regular, non-symlink files can be imported or cleaned up.".into());
    }
    let modified = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{}:{modified}",
            metadata.dev(),
            metadata.ino(),
            metadata.len()
        ))
    }
    #[cfg(not(unix))]
    {
        let created = metadata
            .created()
            .map_err(|e| e.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        Ok(format!("{created}:{}:{modified}", metadata.len()))
    }
}

pub fn kind(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "tif" | "tiff" | "bmp" | "gif" => Some("photo"),
        "heic" | "heif" | "avif" => Some("heic"),
        "cr2" | "cr3" | "crw" | "nef" | "nrw" | "arw" | "srf" | "sr2" | "dng" | "raf" | "orf"
        | "rw2" | "pef" | "ptx" | "raw" | "rwl" | "3fr" | "iiq" | "srw" | "kdc" | "dcr" | "mos"
        | "mrw" | "erf" | "x3f" => Some("raw"),
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" | "mts" | "m2ts" | "3gp" | "mpg"
        | "mpeg" | "wmv" => Some("video"),
        "xmp" => Some("sidecar"),
        _ => None,
    }
}

pub fn value<'a>(metadata: &'a serde_json::Value, tag: &str) -> Option<&'a serde_json::Value> {
    metadata
        .as_object()?
        .iter()
        .find(|(key, _)| key.split(':').next_back() == Some(tag))
        .map(|(_, value)| value)
}

fn text(metadata: &serde_json::Value, tag: &str) -> Option<String> {
    value(metadata, tag).map(|v| {
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string())
    })
}

pub fn valid_date(input: &str) -> Option<String> {
    let normalized = input.trim().replace(' ', "T");
    let normalized = if normalized.as_bytes().get(4) == Some(&b':') {
        format!(
            "{}-{}-{}",
            normalized.get(..4)?,
            normalized.get(5..7)?,
            normalized.get(8..)?
        )
    } else {
        normalized
    };
    let day = NaiveDate::parse_from_str(normalized.get(..10)?, "%Y-%m-%d").ok()?;
    if matches!(
        day.to_string().as_str(),
        "1970-01-01" | "1904-01-01" | "1980-01-01"
    ) || day > Utc::now().date_naive().succ_opt()?
    {
        return None;
    }
    if normalized.len() == 10 {
        return Some(day.to_string());
    }
    NaiveDateTime::parse_from_str(normalized.get(..19)?, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
}

pub fn select_date(
    metadata: &serde_json::Value,
    birth: Option<&str>,
    modified: Option<&str>,
) -> (Option<String>, String, Vec<String>) {
    let mut warnings = Vec::new();
    let mut candidates = Vec::new();
    for tag in [
        "DateTimeOriginal",
        "SubSecDateTimeOriginal",
        "CreationDate",
        "CreateDate",
        "MediaCreateDate",
    ] {
        if let Some(raw) = text(metadata, tag) {
            if let Some(date) = valid_date(&raw) {
                candidates.push((date, format!("capture metadata: {tag}")));
            } else {
                warnings.push(format!("Ignored invalid/default/future {tag}: {raw}"));
            }
        }
    }
    if let Some((date, source)) = candidates.first() {
        if candidates
            .iter()
            .any(|(other, _)| other.get(..10) != date.get(..10))
        {
            warnings.push("Capture-date fields disagree; review this date.".into());
        }
        if text(metadata, "FileType").is_some_and(|x| ["MOV", "MP4", "M4V"].contains(&x.as_str()))
            && value(metadata, "OffsetTimeOriginal").is_none()
            && value(metadata, "CreationDate").is_none()
        {
            warnings.push(
                "Video creation time has no timezone; kept its recorded calendar date.".into(),
            );
        }
        return (Some(date.clone()), source.clone(), warnings);
    }
    for (date, source) in [
        (birth, "estimated: filesystem creation"),
        (modified, "estimated: file modification"),
    ] {
        if let Some(date) = date.and_then(valid_date) {
            return (Some(date), source.into(), warnings);
        }
    }
    warnings.push("No usable date. Set the date manually if known.".into());
    (None, "unknown".into(), warnings)
}

fn reader(path: &Path) -> Result<DynamicImage> {
    let mut reader = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|e| format!("Cannot decode image: {e}"))
}

pub fn decode(
    app: &tauri::AppHandle,
    path: &Path,
    kind: &str,
    metadata: &serde_json::Value,
) -> Result<DynamicImage> {
    let image = match kind {
        "photo" => reader(path)?,
        "raw" => {
            let mut command = Command::new(tool(app, "dcraw_emu")?);
            command.args(["-w", "-T", "-Z", "-"]).arg(path);
            let bytes = execute(command, 180)?;
            let mut output = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            output.write_all(&bytes).map_err(|e| e.to_string())?;
            return reader(output.path());
        }
        "heic" => {
            let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
            let output = directory.path().join("decoded.png");
            let mut command = Command::new(tool(app, "heif-convert")?);
            command.arg(path).arg(&output);
            execute(command, 120)?;
            return reader(&output);
        }
        "video" => {
            let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
            let output = directory.path().join("frame.jpg");
            let mut command = Command::new(tool(app, "ffmpeg")?);
            command
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(path)
                .args(["-frames:v", "1", "-vf", "scale=960:-2", "-y"])
                .arg(&output);
            execute(command, 120)?;
            return reader(&output);
        }
        _ => return Err("Sidecars have no image preview.".into()),
    };
    let orientation = value(metadata, "Orientation")
        .and_then(|v| v.as_u64())
        .unwrap_or(1);
    Ok(match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    })
}

pub fn signature(image: &DynamicImage) -> String {
    let small = image
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash = (hash << 1) | u64::from(small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0]);
        }
    }
    format!("{hash:016x}")
}

pub fn distance(left: &str, right: &str) -> Option<u32> {
    Some((u64::from_str_radix(left, 16).ok()? ^ u64::from_str_radix(right, 16).ok()?).count_ones())
}

pub fn inspect(app: &tauri::AppHandle, path: &Path) -> Result<Media> {
    let kind = kind(path).ok_or("Unsupported media extension.")?;
    let before = identity(path)?;
    let stat = fs::metadata(path).map_err(|e| e.to_string())?;
    let hash = hash(path)?;
    let mut warnings = Vec::new();
    let metadata = if kind == "sidecar" {
        serde_json::json!({})
    } else {
        let result = (|| {
            let mut command = Command::new(tool(app, "exiftool")?);
            command
                .args(["-json", "-G1", "-n", "-api", "LargeFileSupport=1", "--"])
                .arg(path);
            let bytes = execute(command, 60)?;
            let rows: Vec<serde_json::Value> =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            rows.into_iter()
                .next()
                .ok_or_else(|| "Metadata tool returned no result.".to_string())
        })();
        match result {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!("Metadata unavailable: {error}"));
                serde_json::json!({})
            }
        }
    };
    let birth = stat.created().ok().map(|d| {
        DateTime::<Local>::from(d)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    });
    let modified = stat.modified().ok().map(|d| {
        DateTime::<Local>::from(d)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    });
    let (date, date_source, date_warnings) =
        select_date(&metadata, birth.as_deref(), modified.as_deref());
    warnings.extend(date_warnings);
    let mut width = value(&metadata, "ImageWidth")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let mut height = value(&metadata, "ImageHeight")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let signature = if kind == "sidecar" {
        None
    } else {
        match decode(app, path, kind, &metadata) {
            Ok(image) => {
                if kind != "video" {
                    (width, height) = image.dimensions();
                }
                Some(signature(&image))
            }
            Err(error) => {
                warnings.push(format!("Preview/similarity unavailable: {error}"));
                None
            }
        }
    };
    if identity(path)? != before {
        return Err("Source changed while it was being inspected; retry it.".into());
    }
    Ok(Media {
        kind: kind.into(),
        date,
        date_source,
        warnings,
        width,
        height,
        hash,
        bytes: stat.len(),
        signature,
        metadata,
    })
}

pub fn photo_equivalent(left: &DynamicImage, right: &DynamicImage) -> bool {
    if left.color().has_alpha() || right.color().has_alpha() {
        return false;
    }
    let (lw, lh) = left.dimensions();
    let (rw, rh) = right.dimensions();
    if lh == 0 || rh == 0 || (lw as f64 / lh as f64 - rw as f64 / rh as f64).abs() > 0.001 {
        return false;
    }
    let a = left
        .resize_exact(512, 512, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let b = right
        .resize_exact(512, 512, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mean = a.as_raw().iter().map(|&x| x as f64).sum::<f64>() / a.as_raw().len() as f64;
    let variance = a
        .as_raw()
        .iter()
        .map(|&x| (x as f64 - mean).powi(2))
        .sum::<f64>()
        / a.as_raw().len() as f64;
    if variance < 100.0 {
        return false;
    }
    // Require agreement in every tile, not just a low whole-image average.
    for ty in 0..8 {
        for tx in 0..8 {
            let mut error = 0.0;
            for y in ty * 64..(ty + 1) * 64 {
                for x in tx * 64..(tx + 1) * 64 {
                    for c in 0..3 {
                        error +=
                            (a.get_pixel(x, y)[c] as f64 - b.get_pixel(x, y)[c] as f64).powi(2);
                    }
                }
            }
            if error / (64.0 * 64.0 * 3.0) > 2.0 {
                return false;
            }
        }
    }
    true
}

pub fn video_equivalent(app: &tauri::AppHandle, left: &Path, right: &Path) -> Result<bool> {
    fn probe(app: &tauri::AppHandle, path: &Path) -> Result<serde_json::Value> {
        let mut command = Command::new(tool(app, "ffprobe")?);
        command
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(path);
        serde_json::from_slice(&execute(command, 60)?).map_err(|e| e.to_string())
    }
    let a = probe(app, left)?;
    let b = probe(app, right)?;
    let streams = |v: &serde_json::Value| v["streams"].as_array().cloned().unwrap_or_default();
    let sa = streams(&a);
    let sb = streams(&b);
    if sa.len() != sb.len() || sa.is_empty() {
        return Ok(false);
    }
    let duration = |v: &serde_json::Value| {
        v["format"]["duration"]
            .as_str()
            .and_then(|x| x.parse::<f64>().ok())
    };
    match (duration(&a), duration(&b)) {
        (Some(a), Some(b)) if (a - b).abs() < 0.01 && a > 0.0 => {}
        _ => return Ok(false),
    }
    let mut video_count = 0;
    let mut frame_count = 0usize;
    for (a, b) in sa.iter().zip(&sb) {
        let kind = a["codec_type"].as_str().ok_or("Missing stream type.")?;
        if a["codec_type"] != b["codec_type"] {
            return Ok(false);
        }
        for field in [
            "avg_frame_rate",
            "time_base",
            "sample_rate",
            "channels",
            "channel_layout",
            "color_space",
            "color_transfer",
            "color_primaries",
            "pix_fmt",
            "disposition",
            "tags",
            "side_data_list",
        ] {
            if a[field] != b[field] {
                return Ok(false);
            }
        }
        match kind {
            "video" => {
                video_count += 1;
                let count = a["nb_read_frames"]
                    .as_str()
                    .and_then(|x| x.parse::<usize>().ok());
                if count.is_none() || a["nb_read_frames"] != b["nb_read_frames"] {
                    return Ok(false);
                }
                frame_count = count.unwrap_or(0);
            }
            "audio" => {
                let audio_hash = |path: &Path, index: usize| -> Result<Vec<u8>> {
                    let mut command = Command::new(tool(app, "ffmpeg")?);
                    command
                        .args(["-v", "error", "-nostdin", "-i"])
                        .arg(path)
                        .args([
                            "-map",
                            &format!("0:{index}"),
                            "-c:a",
                            "pcm_s32le",
                            "-f",
                            "hash",
                            "-hash",
                            "sha256",
                            "-",
                        ]);
                    execute(command, 3600)
                };
                let index = a["index"].as_u64().ok_or("Missing audio stream index.")? as usize;
                if audio_hash(left, index)? != audio_hash(right, index)? {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    if video_count != 1 {
        return Ok(false);
    }
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let stats = directory.path().join("comparison.log");
    let stats_filter = stats
        .to_string_lossy()
        .replace('\\', "/")
        .replace(':', "\\:");
    let mut command = Command::new(tool(app, "ffmpeg")?);
    command.args(["-v", "error", "-nostdin", "-i"]).arg(left).arg("-i").arg(right)
        .args(["-filter_complex",
            &format!("[0:v]scale=640:360:flags=area,setpts=PTS-STARTPTS[a];[1:v]scale=640:360:flags=area,setpts=PTS-STARTPTS[b];[a][b]ssim=stats_file='{stats_filter}':shortest=1"),
            "-an", "-f", "null", "-"]);
    execute(command, 3600)?;
    let content = fs::read_to_string(stats).map_err(|e| e.to_string())?;
    let mut count = 0;
    for line in content.lines() {
        let score = line
            .split_whitespace()
            .find_map(|part| part.strip_prefix("All:"))
            .and_then(|x| x.parse::<f64>().ok())
            .ok_or("Missing frame comparison score.")?;
        if !score.is_finite() || score < 0.9995 {
            return Ok(false);
        }
        count += 1;
    }
    Ok(count > 0 && count == frame_count)
}
