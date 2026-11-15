use crate::{archive::Archive, media, model::*, paths};
use std::{
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};
use tauri::{Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

pub fn update(app: &tauri::AppHandle, progress: Progress) -> Result<()> {
    let state = app.state::<AppState>();
    *state.progress.lock().map_err(|e| e.to_string())? = progress.clone();
    app.emit("progress", &progress).map_err(|e| e.to_string())
}

pub fn run(app: &tauri::AppHandle, job: &QueuedJob) -> Result<()> {
    let state = app.state::<AppState>();
    let archive_root = {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive
            .as_ref()
            .ok_or("Choose an archive first.")?
            .root
            .clone()
    };
    let mut progress = Progress {
        status: "scanning".into(),
        ..Progress::default()
    };
    update(app, progress.clone())?;
    let mut roots = Vec::<PathBuf>::new();
    for root in &job.roots {
        let root = Path::new(root);
        if root
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Dropped roots must not be symlinks.".into());
        }
        let root = root
            .canonicalize()
            .map_err(|e| format!("Cannot open source folder: {e}"))?;
        if !root.is_dir() {
            return Err("Drop folders rather than individual files.".into());
        }
        if root.starts_with(&archive_root) || archive_root.starts_with(&root) {
            return Err("Source and destination folders must not overlap.".into());
        }
        if roots.iter().any(|existing| root.starts_with(existing)) {
            continue;
        }
        roots.retain(|existing| !existing.starts_with(&root));
        roots.push(root);
    }
    let mut files = Vec::new();
    for root in &roots {
        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.file_name() != ".photo-dedupe"
                    && !e
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".photo-dedupe-cleanup-")
            })
        {
            if state.cancel.load(Ordering::Relaxed) {
                break;
            }
            match entry {
                Ok(entry) if entry.file_type().is_file() && media::kind(entry.path()).is_some() => {
                    files.push((root.clone(), entry.into_path()));
                }
                Ok(_) => {}
                Err(error) => progress.errors.push(format!("Folder scan: {error}")),
            }
        }
    }
    // Import image companions before XMP so sidecars can inherit their owner's date/path.
    files.sort_by_key(|(_, path)| media::kind(path) == Some("sidecar"));
    progress.total = files.len();
    progress.status = "importing".into();
    update(app, progress.clone())?;
    for (root, path) in files {
        if state.cancel.load(Ordering::Relaxed) {
            break;
        }
        while state.pause.load(Ordering::Relaxed) && !state.cancel.load(Ordering::Relaxed) {
            progress.status = "paused".into();
            update(app, progress.clone())?;
            std::thread::sleep(Duration::from_millis(200));
        }
        if state.cancel.load(Ordering::Relaxed) {
            break;
        }
        progress.status = "importing".into();
        progress.current = path.to_string_lossy().into();
        update(app, progress.clone())?;
        let result = (|| {
            let identity = media::identity(&path)?;
            let media = media::inspect(app, &path)?;
            let mut guard = state.archive.lock().map_err(|e| e.to_string())?;
            let archive = guard.as_mut().ok_or("Archive is not open.")?;
            import_one(app, archive, &job.settings, &root, &path, &identity, media)
        })();
        match result {
            Ok(true) => progress.duplicates += 1,
            Ok(false) => progress.copied += 1,
            Err(error) => progress.errors.push(format!("{}: {error}", path.display())),
        }
        progress.done += 1;
        update(app, progress.clone())?;
        let guard = state.archive.lock().map_err(|e| e.to_string())?;
        guard.as_ref().ok_or("Archive is not open.")?.save_job(&job.id,
            &serde_json::json!({"id":job.id, "roots":job.roots, "settings":job.settings, "progress":progress}))?;
    }
    progress.current.clear();
    progress.status = if state.cancel.load(Ordering::Relaxed) {
        "cancelled"
    } else if progress.errors.is_empty() {
        "complete"
    } else {
        "complete with errors"
    }
    .into();
    {
        let mut guard = state.archive.lock().map_err(|e| e.to_string())?;
        let archive = guard.as_mut().ok_or("Archive is not open.")?;
        if let Err(error) = archive.backup() {
            progress
                .errors
                .push(format!("Catalog recovery backup failed: {error}"));
        }
        archive.save_job(&job.id, &serde_json::json!({"id":job.id, "roots":job.roots, "settings":job.settings, "progress":progress}))?;
    }
    update(app, progress)
}

fn import_one(
    app: &tauri::AppHandle,
    archive: &mut Archive,
    settings: &Settings,
    root: &Path,
    source: &Path,
    identity: &str,
    mut media: Media,
) -> Result<bool> {
    archive.check()?;
    let album = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let occurrence_id = archive
        .occurrences()?
        .iter()
        .find(|o| o.source == source.to_string_lossy() && o.hash == media.hash)
        .map(|o| o.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let occurrence = |item: String| Occurrence {
        id: occurrence_id.clone(),
        item,
        source: source.to_string_lossy().into(),
        root: root.to_string_lossy().into(),
        album: album.clone(),
        relative: source
            .strip_prefix(root)
            .unwrap_or(source)
            .to_string_lossy()
            .into(),
        hash: media.hash.clone(),
        identity: identity.into(),
        status: "pending".into(),
        bytes: media.bytes,
    };
    if let Some(exact) = archive.exact(&media.hash)? {
        if media::identity(source)? != identity {
            return Err("Source changed before exact deduplication.".into());
        }
        archive.record_occurrence(&occurrence(exact.id))?;
        return Ok(true);
    }
    let id = Uuid::new_v4().to_string();
    let original_name = source
        .file_name()
        .ok_or("Missing source filename.")?
        .to_string_lossy()
        .to_string();
    let owner = if media.kind == "sidecar" {
        archive
            .occurrences()?
            .into_iter()
            .find(|o| {
                let other = Path::new(&o.source);
                other.parent() == source.parent()
                    && other.file_stem() == source.file_stem()
                    && media::kind(other) != Some("sidecar")
            })
            .map(|o| archive.item(&o.item))
            .transpose()?
    } else {
        None
    };
    if let Some(owner) = &owner {
        media.date = owner.media.date.clone();
        media.date_source = owner.media.date_source.clone();
    }
    let candidates = if media.kind == "sidecar" {
        Vec::new()
    } else {
        archive.candidates(&media)?
    };
    let mut matched: Option<(Item, bool)> = None;
    let mut review = Vec::new();
    for candidate in candidates {
        let path = paths::inside(&archive.root, Path::new(&candidate.path))?;
        if !path.is_file() || media::hash(&path)? != candidate.media.hash {
            media.warnings.push(format!(
                "Candidate {} is missing or changed; it did not suppress this import.",
                candidate.original_name
            ));
            continue;
        }
        let protected = media.kind == "raw"
            || source.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
            || media::value(&media.metadata, "BitsPerSample") != media::value(&candidate.media.metadata, "BitsPerSample")
            || !archive.companions(&candidate.id)?.is_empty()
            || media::value(&media.metadata, "ContentIdentifier").is_some()
            || media::value(&candidate.media.metadata, "ContentIdentifier").is_some()
            || (media.date_source.starts_with("capture")
                && candidate.media.date_source.starts_with("capture")
                && media.date != candidate.media.date)
            || media::value(&media.metadata, "ProfileDescription")
                != media::value(&candidate.media.metadata, "ProfileDescription");
        let dominates =
            media.width > candidate.media.width && media.height > candidate.media.height;
        let dominated =
            media.width < candidate.media.width && media.height < candidate.media.height;
        let auto = settings.automatic_matches && !protected && (dominates || dominated);
        let comparison: Result<bool> = if auto {
            if media.kind == "video" {
                media::video_equivalent(app, source, &path)
            } else {
                (|| {
                    let left = media::decode(app, source, &media.kind, &media.metadata)?;
                    let right = media::decode(app, &path, &candidate.media.kind, &candidate.media.metadata)?;
                    Ok(media::photo_equivalent(&left, &right))
                })()
            }
        } else {
            Ok(false)
        };
        let equivalent = match comparison {
            Ok(equivalent) => equivalent,
            Err(error) => {
                media.warnings.push(format!("Automatic comparison unavailable; original retained for review: {error}"));
                false
            }
        };
        if equivalent {
            matched = Some((candidate, dominates));
            break;
        }
        review.push((candidate.id, if protected {
            "Related scene or protected RAW/JPEG/edit/Live Photo variant. Keep both unless you are certain."
        } else { "Similar visual fingerprint. Quality/equivalence is uncertain; compare before choosing." }.to_string()));
    }
    let is_lower = matched.as_ref().is_some_and(|(_, dominates)| !dominates);
    let wanted = if is_lower {
        PathBuf::from(".photo-dedupe/undo")
            .join(&id)
            .join(paths::safe_name(&original_name))
    } else if let Some(owner) = &owner {
        PathBuf::from(&owner.path).with_extension("xmp")
    } else {
        paths::relative(settings, &media, &original_name)?
    };
    let path = paths::unique(&archive.root, &wanted, &id)?;
    if fs2::available_space(&archive.root).map_err(|e| e.to_string())?
        < media.bytes.saturating_mul(3)
    {
        return Err("Destination needs more free space for verified staging and undo.".into());
    }
    if media::identity(source)? != identity {
        return Err("Source changed before copying.".into());
    }
    let mut item = Item {
        id: id.clone(),
        path: path.to_string_lossy().replace('\\', "/"),
        original_name,
        state: if is_lower { "undo" } else { "active" }.into(),
        keeper: matched
            .as_ref()
            .filter(|(_, d)| !d)
            .map(|(item, _)| item.id.clone()),
        media: media.clone(),
        settings: settings.clone(),
        albums: vec![album.clone()],
    };
    if matched.is_some() {
        item.media.warnings.push("Automatically grouped after full-frame equivalence checks; the higher-resolution copy is active. Original bytes remain in archive undo.".into());
    }
    archive.place(source, item, Some(occurrence(id.clone())), None)?;
    if let Some((old, dominates)) = matched
        && dominates
    {
        archive.move_to_undo(&old.id, &id)?;
    }
    for (candidate, reason) in review {
        archive.add_review(&id, &candidate, &reason)?;
    }
    Ok(false)
}
