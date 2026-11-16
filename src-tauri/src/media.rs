use crate::model::{Media, Result};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use image::{DynamicImage, GenericImageView, ImageReader};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, Cursor, Read, Seek, Write},
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

pub fn execute(command: Command, timeout: u64) -> Result<Vec<u8>> {
    execute_output(command, timeout, false)
}

fn execute_output(mut command: Command, timeout: u64, metadata_batch: bool) -> Result<Vec<u8>> {
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
    // ExifTool returns 1 for mixed batches and can omit failed files from JSON.
    // Preserve valid dates, log stderr, and let callers report omitted files.
    if !status.success() {
        let mut message = String::new();
        errors
            .reopen()
            .map_err(|e| e.to_string())?
            .take(8192)
            .read_to_string(&mut message)
            .map_err(|e| e.to_string())?;
        if metadata_batch && status.code() == Some(1) {
            eprintln!("Metadata batch reported errors: {}", message.trim());
        } else {
            return Err(format!("Media tool failed ({status}): {}", message.trim()));
        }
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
    hash_reader(&mut file)
}

fn hash_reader(file: &mut File) -> Result<String> {
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

/// Reads and hashes a source file while tolerating one metadata shift caused by
/// cloud-backed placeholders (OneDrive / iCloud FileProvider) hydrating on first read.
pub fn hash_with_identity(path: &Path) -> Result<(String, String)> {
    for attempt in 0..2 {
        let before = identity(path)?;
        let mut file =
            File::open(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let opened = identity_from_metadata(&file.metadata().map_err(|e| e.to_string())?)?;
        let hash = hash_reader(&mut file)?;
        let after = identity(path)?;
        if after == before && after == opened {
            return Ok((after, hash));
        }
        // If FileProvider hydrated the placeholder on open/read, `after` now reflects
        // the materialized local file; one immediate re-verify confirms stability.
        if attempt == 0 && after == opened {
            let mut verify =
                File::open(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
            let verify_hash = hash_reader(&mut verify)?;
            let settled = identity(path)?;
            if settled == after && verify_hash == hash {
                return Ok((settled, hash));
            }
        }
    }
    Err("Source changed during hashing; retry it.".into())
}

fn identity_from_metadata(metadata: &fs::Metadata) -> Result<String> {
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

pub fn identity(path: &Path) -> Result<String> {
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Only regular, non-symlink files can be imported or cleaned up.".into());
    }
    identity_from_metadata(&metadata)
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
    if normalized.len() == 10
        || matches!(normalized.as_bytes().get(10), Some(b'Z' | b'+' | b'-'))
    {
        return Some(day.to_string());
    }
    if let Some(full) = normalized
        .get(..19)
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
    {
        return Some(full.format("%Y-%m-%dT%H:%M:%S").to_string());
    }
    normalized
        .get(..16)
        .and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
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
    read_image(ImageReader::open(path).map_err(|e| e.to_string())?)
}

fn read_image<R: BufRead + Seek>(reader: ImageReader<R>) -> Result<DynamicImage> {
    let mut reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
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
    decode_sized(app, path, kind, metadata, 2200)
}

pub fn decode_sized(
    app: &tauri::AppHandle,
    path: &Path,
    kind: &str,
    metadata: &serde_json::Value,
    min_side: u32,
) -> Result<DynamicImage> {
    let image = match kind {
        "photo" => {
            let jpeg = path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")
            });
            if jpeg && let Ok(Some((scaled, _, _))) = scaled_jpeg_min(path, min_side) {
                scaled
            } else {
                reader(path)?
            }
        }
        "raw" => {
            let mut command = Command::new(tool(app, "dcraw_emu")?);
            if min_side <= 1000 {
                command.args(["-w", "-h", "-T", "-Z", "-"]).arg(path);
            } else {
                command.args(["-w", "-T", "-Z", "-"]).arg(path);
            }
            let bytes = execute(command, 180)?;
            let mut output = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            output.write_all(&bytes).map_err(|e| e.to_string())?;
            return reader(output.path());
        }
        "heic" => {
            let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
            let output = directory.path().join("decoded.jpg");
            let mut command = Command::new(tool(app, "heif-convert")?);
            command.arg(path).arg(&output);
            execute(command, 120)?;
            if output.is_file() {
                return reader(&output);
            }
            let mut produced = fs::read_dir(directory.path())
                .map_err(|e| e.to_string())?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg"))
                })
                .collect::<Vec<_>>();
            produced.sort();
            let first = produced
                .first()
                .ok_or("HEIC decoder produced no readable image.")?;
            return reader(first);
        }
        "video" => {
            let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
            let output = directory.path().join("frame.jpg");
            let ffmpeg = tool(app, "ffmpeg")?;
            let mut seeked = Command::new(&ffmpeg);
            seeked
                .args(["-v", "error", "-nostdin", "-ss", "1", "-i"])
                .arg(path)
                .args(["-frames:v", "1", "-vf", "scale=960:-2", "-y"])
                .arg(&output);
            let _ = execute(seeked, 120);
            if !output.is_file() {
                let mut fallback = Command::new(ffmpeg);
                fallback
                    .args(["-v", "error", "-nostdin", "-i"])
                    .arg(path)
                    .args(["-frames:v", "1", "-vf", "scale=960:-2", "-y"])
                    .arg(&output);
                execute(fallback, 120)?;
            }
            return reader(&output);
        }
        _ => return Err("Sidecars have no image preview.".into()),
    };
    Ok(orient(image, metadata))
}

fn orientation(metadata: &serde_json::Value) -> u64 {
    value(metadata, "Orientation")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
}

fn orient(image: DynamicImage, metadata: &serde_json::Value) -> DynamicImage {
    match orientation(metadata) {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Side length of the comparison grid; reduced decodes never go below it.
const COMPARE: u32 = 512;

/// Reduced-size decode used for fingerprints and pixel comparison.
#[derive(Clone)]
pub struct Preview {
    pub pixels: image::RgbImage,
    pub alpha: bool,
    /// Full-resolution size after orientation.
    pub width: u32,
    pub height: u32,
    pub signature: Option<String>,
}

/// JPEG DCT scaling decodes at 1/2, 1/4 or 1/8 size, which is much cheaper than a full
/// decode. Returns None for pixel formats the fast path does not handle.
pub(crate) fn scaled_jpeg(path: &Path) -> Result<Option<(DynamicImage, u32, u32)>> {
    scaled_jpeg_min(path, COMPARE)
}

pub(crate) fn scaled_jpeg_min(
    path: &Path,
    min_side: u32,
) -> Result<Option<(DynamicImage, u32, u32)>> {
    let file = std::io::BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    scaled_jpeg_reader(file, min_side)
}

pub(crate) fn scaled_jpeg_reader(
    input: impl Read,
    min_side: u32,
) -> Result<Option<(DynamicImage, u32, u32)>> {
    let mut decoder = jpeg_decoder::Decoder::new(input);
    decoder.read_info().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("Missing JPEG header.")?;
    if info.width > 30_000 || info.height > 30_000 {
        return Err("Image dimensions exceed the safety limit.".into());
    }
    if !matches!(
        info.pixel_format,
        jpeg_decoder::PixelFormat::L8 | jpeg_decoder::PixelFormat::RGB24
    ) {
        return Ok(None);
    }
    // The decoder picks the smallest scale where *either* side reaches the request, so
    // request proportionally to keep the short side at or above the requested minimum.
    let short = u32::from(info.width.min(info.height)).max(1);
    let request = |side: u16| {
        (u32::from(side) * min_side)
            .div_ceil(short)
            .min(u32::from(side)) as u16
    };
    let (width, height) = decoder
        .scale(request(info.width), request(info.height))
        .map_err(|e| e.to_string())?;
    let pixels = decoder.decode().map_err(|e| e.to_string())?;
    let (w, h) = (u32::from(width), u32::from(height));
    let image = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(w, h, pixels).map(DynamicImage::ImageLuma8)
        }
        _ => image::RgbImage::from_raw(w, h, pixels).map(DynamicImage::ImageRgb8),
    };
    Ok(image.map(|image| (image, u32::from(info.width), u32::from(info.height))))
}

fn shrink(image: DynamicImage) -> DynamicImage {
    let (w, h) = image.dimensions();
    let short = w.min(h);
    if short <= COMPARE * 2 {
        return image;
    }
    let scale = f64::from(COMPARE) / f64::from(short);
    let nw = ((f64::from(w) * scale).ceil() as u32).max(COMPARE);
    let nh = ((f64::from(h) * scale).ceil() as u32).max(COMPARE);
    image.thumbnail_exact(nw, nh)
}

pub fn preview(
    app: &tauri::AppHandle,
    path: &Path,
    kind: &str,
    metadata: &serde_json::Value,
) -> Result<Preview> {
    preview_input(app, path, kind, metadata, None)
}

fn preview_input(
    app: &tauri::AppHandle,
    path: &Path,
    kind: &str,
    metadata: &serde_json::Value,
    bytes: Option<&[u8]>,
) -> Result<Preview> {
    let jpeg = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"));
    let fast = if kind == "photo" && jpeg {
        match bytes {
            Some(bytes) => scaled_jpeg_reader(Cursor::new(bytes), COMPARE),
            None => scaled_jpeg(path),
        }
        .unwrap_or(None)
    } else {
        None
    };
    let (mut image, mut width, mut height) = match (fast, kind) {
        (Some(fast), _) => fast,
        (None, "photo") => {
            let image = match bytes {
                Some(bytes) => read_image(ImageReader::new(Cursor::new(bytes)))?,
                None => reader(path)?,
            };
            let (w, h) = image.dimensions();
            (shrink(image), w, h)
        }
        (None, "raw") => {
            // Half-size mode skips demosaicing; it is plenty for a 512px comparison.
            let mut command = Command::new(tool(app, "dcraw_emu")?);
            command.args(["-w", "-h", "-T", "-Z", "-"]).arg(path);
            let bytes = execute(command, 180)?;
            let mut output = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            output.write_all(&bytes).map_err(|e| e.to_string())?;
            let image = reader(output.path())?;
            let (w, h) = image.dimensions();
            (image, w * 2, h * 2)
        }
        // HEIC and video decoders already apply their own rotation.
        (None, _) => {
            let image = decode(app, path, kind, metadata)?;
            let (w, h) = image.dimensions();
            (shrink(image), w, h)
        }
    };
    if kind == "photo" {
        if (5..=8).contains(&orientation(metadata)) {
            std::mem::swap(&mut width, &mut height);
        }
        image = orient(image, metadata);
    }
    Ok(Preview::new(&image, width, height))
}

fn pixel_variance(pixels: &image::RgbImage) -> f64 {
    let raw = pixels.as_raw();
    if raw.is_empty() {
        return 0.0;
    }
    let mean = raw.iter().map(|&x| x as f64).sum::<f64>() / raw.len() as f64;
    raw.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / raw.len() as f64
}

impl Preview {
    pub fn new(image: &DynamicImage, width: u32, height: u32) -> Self {
        let pixels = image.thumbnail_exact(COMPARE, COMPARE).to_rgb8();
        let signature = signature_from_pixels(&pixels);
        Self {
            alpha: image.color().has_alpha(),
            signature,
            pixels,
            width,
            height,
        }
    }
}

/// Area-averages the 512x512 preview into an 8x8 luma grid and encodes 32 horizontal + 32 vertical
/// block gradients. Requires minimum image variance and non-degenerate bit balance so mostly-blank
/// UI screenshots, white documents, and black fade-in frames do not collide with unrelated images.
fn signature_from_pixels(pixels: &image::RgbImage) -> Option<String> {
    if pixel_variance(pixels) < 16.0 {
        return None;
    }
    let mut grid = [[0.0f64; 8]; 8];
    let raw = pixels.as_raw();
    let stride = COMPARE as usize * 3;
    for (gy, row) in grid.iter_mut().enumerate() {
        for (gx, cell) in row.iter_mut().enumerate() {
            let mut sum = 0u64;
            for y in gy * 64..(gy + 1) * 64 {
                let row_start = y * stride + gx * 64 * 3;
                for px in raw[row_start..row_start + 64 * 3].as_chunks::<3>().0 {
                    sum += 299 * u64::from(px[0]) + 587 * u64::from(px[1]) + 114 * u64::from(px[2]);
                }
            }
            *cell = sum as f64 / (64.0 * 64.0 * 1000.0);
        }
    }
    let mut hash = 0u64;
    for row in &grid {
        for x in 0..4 {
            let left = (row[x * 2] + row[x * 2 + 1]) * 0.5;
            let right = (row[((x + 1) * 2) % 8] + row[((x + 1) * 2 + 1) % 8]) * 0.5;
            hash = (hash << 1) | u64::from(left > right + 0.5);
        }
    }
    for y in 0..4 {
        let next_y = ((y + 1) * 2) % 8;
        for ((&t0, &t1), (&b0, &b1)) in grid[y * 2]
            .iter()
            .zip(&grid[y * 2 + 1])
            .zip(grid[next_y].iter().zip(&grid[next_y + 1]))
        {
            let top = (t0 + t1) * 0.5;
            let bottom = (b0 + b1) * 0.5;
            hash = (hash << 1) | u64::from(top > bottom + 0.5);
        }
    }
    let ones = hash.count_ones();
    if !(6..=58).contains(&ones) {
        return None;
    }
    Some(format!("{hash:016x}"))
}

pub fn aspect_compatible(w1: u32, h1: u32, w2: u32, h2: u32) -> bool {
    if w1 == 0 || h1 == 0 || w2 == 0 || h2 == 0 {
        return true;
    }
    let r1 = f64::from(w1) / f64::from(h1);
    let r2 = f64::from(w2) / f64::from(h2);
    r1.max(r2) / r1.min(r2) <= 1.25
}

pub fn distance(left: &str, right: &str) -> Option<u32> {
    Some((u64::from_str_radix(left, 16).ok()? ^ u64::from_str_radix(right, 16).ok()?).count_ones())
}

fn normalize_source_key(text: &str) -> String {
    let replaced = text.replace('\\', "/");
    let stripped = replaced.strip_prefix("//?/").unwrap_or(&replaced);
    if cfg!(windows) {
        stripped.to_lowercase()
    } else {
        stripped.to_string()
    }
}

pub fn metadata_batch(
    app: &tauri::AppHandle,
    paths: &[PathBuf],
) -> Result<std::collections::HashMap<PathBuf, serde_json::Value>> {
    if paths.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let mut lookup = std::collections::HashMap::with_capacity(paths.len());
    for path in paths {
        lookup.insert(normalize_source_key(&path.to_string_lossy()), path.clone());
    }
    let mut command = Command::new(tool(app, "exiftool")?);
    command.args([
        "-config",
        "",
        "-charset",
        "filename=utf8",
        "-json",
        "-G1",
        "-n",
        "-api",
        "LargeFileSupport=1",
        "-DateTimeOriginal",
        "-SubSecDateTimeOriginal",
        "-CreationDate",
        "-CreateDate",
        "-MediaCreateDate",
        "-OffsetTimeOriginal",
        "-FileType",
        "-ImageWidth",
        "-ImageHeight",
        "-Duration",
        "-Orientation",
        "-ContentIdentifier",
        "-BitsPerSample",
        "-ProfileDescription",
        "-Error",
        "-Warning",
        "--",
    ]);
    command.args(paths);
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&execute_output(command, 180, true)?)
        .map_err(|e| format!("Invalid metadata batch response: {e}"))?;
    let mut result = std::collections::HashMap::with_capacity(rows.len());
    for row in rows {
        let source = value(&row, "SourceFile")
            .and_then(|v| v.as_str())
            .ok_or("Metadata result has no source filename.")?;
        let key = lookup
            .remove(&normalize_source_key(source))
            .unwrap_or_else(|| PathBuf::from(source));
        result.insert(key, row);
    }
    Ok(result)
}

pub fn prepare_inspection(
    path: &Path,
    hash: String,
    metadata: Result<serde_json::Value>,
) -> Result<Media> {
    let kind = kind(path).ok_or("Unsupported media extension.")?;
    let stat = fs::metadata(path).map_err(|e| e.to_string())?;
    let mut warnings = Vec::new();
    let metadata = if kind == "sidecar" {
        serde_json::json!({})
    } else {
        match metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!("Metadata unavailable: {error}"));
                serde_json::json!({})
            }
        }
    };
    for tag in ["Error", "Warning"] {
        if let Some(warning) = value(&metadata, tag) {
            warnings.push(format!("Metadata {tag}: {warning}"));
        }
    }
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
    let width = value(&metadata, "ImageWidth")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let height = value(&metadata, "ImageHeight")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let duration_seconds = value(&metadata, "Duration")
        .and_then(|value| value.as_f64())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0);
    Ok(Media {
        kind: kind.into(),
        date,
        date_source,
        warnings,
        width,
        height,
        duration_seconds,
        hash,
        bytes: stat.len(),
        signature: None,
        metadata,
    })
}

pub fn inspect(
    app: &tauri::AppHandle,
    path: &Path,
    mut media: Media,
    bytes: Option<&[u8]>,
) -> (Media, Option<Preview>) {
    let preview = if media.kind == "sidecar" {
        None
    } else {
        match preview_input(app, path, &media.kind, &media.metadata, bytes) {
            Ok(preview) => {
                if media.kind != "video" {
                    (media.width, media.height) = (preview.width, preview.height);
                }
                Some(preview)
            }
            Err(error) => {
                media
                    .warnings
                    .push(format!("Preview/similarity unavailable: {error}"));
                None
            }
        }
    };
    media.signature = preview.as_ref().and_then(|p| p.signature.clone());
    (media, preview)
}

pub fn photo_equivalent(left: &Preview, right: &Preview) -> bool {
    if left.alpha || right.alpha {
        return false;
    }
    let (lw, lh) = (left.width, left.height);
    let (rw, rh) = (right.width, right.height);
    if lh == 0 || rh == 0 {
        return false;
    }
    let ratio_a = lw as f64 / lh as f64;
    let ratio_b = rw as f64 / rh as f64;
    let rounding_slack = ((ratio_a.max(1.0)) / f64::from(lh.min(rh)) + 0.001).min(0.005);
    if (ratio_a - ratio_b).abs() > rounding_slack {
        return false;
    }
    let (a, b) = (&left.pixels, &right.pixels);
    if pixel_variance(a) < 100.0 {
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

/// Second-stage visual check before bothering the user in Similarity Review.
/// Keeps genuine bursts, edits, tone-mapped RAW/JPEGs, and modest crops while filtering out
/// coincidental dHash collisions (such as two unrelated white UI screenshots).
pub fn visually_similar(left: &Preview, right: &Preview) -> bool {
    if !aspect_compatible(left.width, left.height, right.width, right.height) {
        return false;
    }
    let (a, b) = (left.pixels.as_raw(), right.pixels.as_raw());
    let stride = COMPARE as usize * 3;
    let mut close_tiles = 0u32;
    let mut total_mse = 0.0f64;
    let mut sa = 0.0f64;
    let mut sb = 0.0f64;
    let mut saa = 0.0f64;
    let mut sbb = 0.0f64;
    let mut sab = 0.0f64;
    for ty in 0..8 {
        for tx in 0..8 {
            let mut tile_err = 0.0f64;
            let mut tile_a = 0.0f64;
            let mut tile_b = 0.0f64;
            for y in ty * 64..(ty + 1) * 64 {
                let start = y * stride + tx * 64 * 3;
                for (pa, pb) in a[start..start + 64 * 3]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(b[start..start + 64 * 3].as_chunks::<3>().0)
                {
                    for (&ca, &cb) in pa.iter().zip(pb) {
                        let d = f64::from(ca) - f64::from(cb);
                        tile_err += d * d;
                    }
                    tile_a += 0.299 * f64::from(pa[0])
                        + 0.587 * f64::from(pa[1])
                        + 0.114 * f64::from(pa[2]);
                    tile_b += 0.299 * f64::from(pb[0])
                        + 0.587 * f64::from(pb[1])
                        + 0.114 * f64::from(pb[2]);
                }
            }
            let mse = tile_err / (64.0 * 64.0 * 3.0);
            total_mse += mse;
            if mse <= 350.0 {
                close_tiles += 1;
            }
            let ma = tile_a / (64.0 * 64.0);
            let mb = tile_b / (64.0 * 64.0);
            sa += ma;
            sb += mb;
            saa += ma * ma;
            sbb += mb * mb;
            sab += ma * mb;
        }
    }
    let avg_mse = total_mse / 64.0;
    let n = 64.0;
    let var_a = (saa - sa * sa / n).max(0.0);
    let var_b = (sbb - sb * sb / n).max(0.0);
    let denom = (var_a * var_b).sqrt();
    let corr = if denom > 1e-6 {
        (sab - sa * sb / n) / denom
    } else {
        0.0
    };
    (close_tiles >= 44 && avg_mse <= 500.0 && corr >= 0.72)
        || (corr >= 0.88 && avg_mse <= 900.0 && close_tiles >= 32)
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
