use crate::{archive::Archive, media, model::*, paths};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Mutex, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

pub fn update(app: &tauri::AppHandle, progress: &Progress) -> Result<()> {
    let state = app.state::<AppState>();
    let mut previous = state.progress.lock().map_err(|e| e.to_string())?;
    let mut updated = state.progress_updated.lock().map_err(|e| e.to_string())?;
    if previous.status == progress.status && updated.elapsed() < Duration::from_millis(250) {
        return Ok(());
    }
    *previous = progress.clone();
    *updated = Instant::now();
    app.emit("progress", progress).map_err(|e| e.to_string())
}

pub fn validate_roots(raw_roots: &[String], archive_root: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::<PathBuf>::new();
    for root in raw_roots {
        let root = Path::new(root);
        if root
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("Dropped roots must not be symlinks.".into());
        }
        let root = paths::canonical(root).map_err(|e| format!("Cannot open source folder: {e}"))?;
        if !root.is_dir() {
            return Err("Drop folders rather than individual files.".into());
        }
        if root.starts_with(archive_root) || archive_root.starts_with(&root) {
            return Err("Source and destination folders must not overlap.".into());
        }
        if roots.iter().any(|existing| root.starts_with(existing)) {
            continue;
        }
        roots.retain(|existing| !existing.starts_with(&root));
        roots.push(root);
    }
    Ok(roots)
}

pub fn run(app: &tauri::AppHandle, job: &QueuedJob) -> Result<()> {
    let state = app.state::<AppState>();
    let archive_root = {
        let mut archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive
            .as_mut()
            .ok_or("Choose an archive first.")?
            .begin_import();
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
    update(app, &progress)?;
    let roots = validate_roots(&job.roots, &archive_root)?;
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
    update(app, &progress)?;
    let mut checkpoint = Instant::now();
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(1))
        .clamp(1, 8);
    let shared: &AppState = &state;
    let (task_tx, task_rx) = mpsc::sync_channel::<Task>(workers * 2);
    let task_rx = Mutex::new(task_rx);
    std::thread::scope(|scope| -> Result<()> {
        let (done_tx, done_rx) = mpsc::channel::<(usize, Inspected)>();
        for _ in 0..workers {
            let (task_rx, done_tx) = (&task_rx, done_tx.clone());
            scope.spawn(move || {
                loop {
                    let task = match task_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => break,
                    };
                    let Ok(task) = task else { break };
                    let result = if shared.cancel.load(Ordering::Relaxed) {
                        Err("Import cancelled.".into())
                    } else {
                        media::inspect(app, &task.path, task.hash, task.metadata)
                    };
                    if done_tx.send((task.index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        let mut queue = VecDeque::<Entry>::new();
        let mut candidate_cache = HashMap::<String, media::Preview>::new();
        let mut base = 0;
        let receive = |queue: &mut VecDeque<Entry>, base: usize, (index, result)| {
            if let Some(entry) = queue.get_mut(index - base) {
                entry.stage = Stage::Inspected(Box::new(result));
            }
        };
        for batch in files.chunks(64) {
            if shared.cancel.load(Ordering::Relaxed) {
                break;
            }
            enum Prepped<'b> {
                Finished(&'b Path, &'b Path, Result<bool>),
                NeedsInspect(&'b Path, &'b Path, String, String),
            }
            let mut prepped = Vec::with_capacity(batch.len());
            let mut metadata_paths = Vec::new();
            for (root, path) in batch {
                if shared.cancel.load(Ordering::Relaxed) {
                    break;
                }
                while shared.pause.load(Ordering::Relaxed) && !shared.cancel.load(Ordering::Relaxed)
                {
                    progress.status = "paused".into();
                    update(app, &progress)?;
                    std::thread::sleep(Duration::from_millis(200));
                }
                if shared.cancel.load(Ordering::Relaxed) {
                    break;
                }
                progress.status = "importing".into();
                progress.current = path.to_string_lossy().into();
                update(app, &progress)?;
                // Disk reads and catalog writes stay on this thread so external drives read
                // sequentially; exact duplicates skip ExifTool and decoding entirely.
                let checked: Result<Option<(String, String)>> = (|| {
                    let (identity, hash) = media::hash_with_identity(path)?;
                    let mut guard = shared.archive.lock().map_err(|e| e.to_string())?;
                    let archive = guard.as_mut().ok_or("Archive is not open.")?;
                    if let Some(exact) = archive.exact_cached(&hash)? {
                        if media::identity(path)? != identity {
                            return Err(
                                "Source changed before duplicate confirmation; retry it.".into()
                            );
                        }
                        let occ = occurrence(
                            archive,
                            root,
                            path,
                            &identity,
                            &hash,
                            exact.media.bytes,
                            exact.id,
                        )?;
                        archive.record_occurrence(&occ)?;
                        return Ok(None);
                    }
                    Ok(Some((identity, hash)))
                })();
                match checked {
                    Err(error) => prepped.push(Prepped::Finished(root, path, Err(error))),
                    Ok(None) => prepped.push(Prepped::Finished(root, path, Ok(true))),
                    Ok(Some((identity, hash))) => {
                        if media::kind(path) != Some("sidecar") {
                            metadata_paths.push(path.clone());
                        }
                        prepped.push(Prepped::NeedsInspect(root, path, identity, hash));
                    }
                }
                while let Ok(done) = done_rx.try_recv() {
                    receive(&mut queue, base, done);
                }
                commit_ready(
                    app,
                    job,
                    &mut queue,
                    &mut base,
                    &mut progress,
                    &mut checkpoint,
                    &mut candidate_cache,
                )?;
            }
            if shared.cancel.load(Ordering::Relaxed) {
                break;
            }
            let metadata = if metadata_paths.is_empty() {
                Ok(HashMap::new())
            } else {
                media::metadata_batch(app, &metadata_paths)
            };
            for item in prepped {
                if shared.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let index = base + queue.len();
                let entry = match item {
                    Prepped::Finished(root, path, result) => Entry {
                        root,
                        path,
                        identity: String::new(),
                        stage: Stage::Finished(result),
                    },
                    Prepped::NeedsInspect(root, path, identity, hash) => {
                        if media::identity(path).ok().as_ref() != Some(&identity) {
                            Entry {
                                root,
                                path,
                                identity: String::new(),
                                stage: Stage::Finished(Err(
                                    "Source changed during batch metadata extraction; retry it."
                                        .into(),
                                )),
                            }
                        } else {
                            let row = if media::kind(path) == Some("sidecar") {
                                Ok(serde_json::json!({}))
                            } else {
                                match &metadata {
                                    Ok(rows) => rows.get(path).cloned().ok_or_else(|| {
                                        "Metadata batch omitted this file.".to_string()
                                    }),
                                    Err(error) => Err(error.clone()),
                                }
                            };
                            task_tx
                                .send(Task {
                                    index,
                                    path: path.to_path_buf(),
                                    hash,
                                    metadata: row,
                                })
                                .map_err(|_| "Decoder workers stopped unexpectedly.")?;
                            Entry {
                                root,
                                path,
                                identity,
                                stage: Stage::Inspecting,
                            }
                        }
                    }
                };
                queue.push_back(entry);
                loop {
                    while let Ok(done) = done_rx.try_recv() {
                        receive(&mut queue, base, done);
                    }
                    commit_ready(
                        app,
                        job,
                        &mut queue,
                        &mut base,
                        &mut progress,
                        &mut checkpoint,
                        &mut candidate_cache,
                    )?;
                    if queue.len() < workers * 4 || shared.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let done = done_rx
                        .recv()
                        .map_err(|_| "Decoder workers stopped unexpectedly.")?;
                    receive(&mut queue, base, done);
                }
            }
        }
        drop(task_tx);
        while !queue.is_empty() && !shared.cancel.load(Ordering::Relaxed) {
            let done = done_rx
                .recv()
                .map_err(|_| "Decoder workers stopped unexpectedly.")?;
            receive(&mut queue, base, done);
            commit_ready(
                app,
                job,
                &mut queue,
                &mut base,
                &mut progress,
                &mut checkpoint,
                &mut candidate_cache,
            )?;
        }
        Ok(())
    })?;
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
    update(app, &progress)
}

type Inspected = Result<(Media, Option<media::Preview>)>;

struct Task {
    index: usize,
    path: PathBuf,
    hash: String,
    metadata: Result<serde_json::Value>,
}

enum Stage {
    Inspecting,
    Inspected(Box<Inspected>),
    Finished(Result<bool>),
}

struct Entry<'a> {
    root: &'a Path,
    path: &'a Path,
    identity: String,
    stage: Stage,
}

/// Commits finished entries in scan order, so XMP sidecars still follow their owners and
/// similar files within one batch are compared against each other deterministically.
fn commit_ready(
    app: &tauri::AppHandle,
    job: &QueuedJob,
    queue: &mut VecDeque<Entry>,
    base: &mut usize,
    progress: &mut Progress,
    checkpoint: &mut Instant,
    candidate_cache: &mut HashMap<String, media::Preview>,
) -> Result<()> {
    let state = app.state::<AppState>();
    while queue
        .front()
        .is_some_and(|entry| !matches!(entry.stage, Stage::Inspecting))
    {
        let entry = queue.pop_front().ok_or("Import queue is empty.")?;
        *base += 1;
        let result = match entry.stage {
            Stage::Finished(result) => result,
            Stage::Inspected(inspected) => (*inspected).and_then(|inspected| {
                let mut guard = state.archive.lock().map_err(|e| e.to_string())?;
                let archive = guard.as_mut().ok_or("Archive is not open.")?;
                import_one(
                    app,
                    archive,
                    &job.settings,
                    entry.root,
                    entry.path,
                    &entry.identity,
                    inspected,
                    candidate_cache,
                )
            }),
            Stage::Inspecting => unreachable!(),
        };
        match result {
            Ok(true) => progress.duplicates += 1,
            Ok(false) => progress.copied += 1,
            Err(error) => progress
                .errors
                .push(format!("{}: {error}", entry.path.display())),
        }
        progress.done += 1;
        update(app, progress)?;
        if checkpoint.elapsed() >= Duration::from_secs(1) {
            let guard = state.archive.lock().map_err(|e| e.to_string())?;
            guard.as_ref().ok_or("Archive is not open.")?.save_job(&job.id,
            &serde_json::json!({"id":job.id, "roots":job.roots, "settings":job.settings, "progress":progress}))?;
            *checkpoint = Instant::now();
        }
    }
    Ok(())
}

fn occurrence(
    archive: &Archive,
    root: &Path,
    source: &Path,
    identity: &str,
    hash: &str,
    bytes: u64,
    item: String,
) -> Result<Occurrence> {
    Ok(Occurrence {
        id: archive
            .occurrence_id(source, hash)?
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        item,
        source: source.to_string_lossy().into(),
        root: root.to_string_lossy().into(),
        album: root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        relative: source
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into(),
        hash: hash.into(),
        identity: identity.into(),
        status: "pending".into(),
        bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn import_one(
    app: &tauri::AppHandle,
    archive: &mut Archive,
    settings: &Settings,
    root: &Path,
    source: &Path,
    identity: &str,
    (mut media, preview): (Media, Option<media::Preview>),
    candidate_cache: &mut HashMap<String, media::Preview>,
) -> Result<bool> {
    archive.check()?;
    let album = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let occurrence_id = archive
        .occurrence_id(source, &media.hash)?
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
    if let Some(exact) = archive.exact_cached(&media.hash)? {
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
        archive.sidecar_owner(source)?
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
        if !archive.verified_file(&path, &candidate.media.hash)? {
            media.warnings.push(format!(
                "Candidate {} is missing or changed; it did not suppress this import.",
                candidate.original_name
            ));
            continue;
        }
        let protected = media.kind == "raw"
            || source
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
            || media::value(&media.metadata, "BitsPerSample")
                != media::value(&candidate.media.metadata, "BitsPerSample")
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
        let auto =
            matched.is_none() && settings.automatic_matches && !protected && (dominates || dominated);
        let mut candidate_preview =
            |candidate: &Item, path: &Path| -> Result<media::Preview> {
                if let Some(cached) = candidate_cache.get(&candidate.id) {
                    return Ok(cached.clone());
                }
                let decoded = media::preview(
                    app,
                    path,
                    &candidate.media.kind,
                    &candidate.media.metadata,
                )?;
                if candidate_cache.len() >= 128 {
                    candidate_cache.clear();
                }
                candidate_cache.insert(candidate.id.clone(), decoded.clone());
                Ok(decoded)
            };
        let (equivalent, plausible): (bool, bool) = if media.kind == "video" {
            let eq = if auto {
                match media::video_equivalent(app, source, &path) {
                    Ok(eq) => eq,
                    Err(error) => {
                        media.warnings.push(format!(
                            "Automatic comparison unavailable; original retained for review: {error}"
                        ));
                        false
                    }
                }
            } else {
                false
            };
            if eq {
                (true, true)
            } else if let Some(left) = preview.as_ref() {
                let sim = candidate_preview(&candidate, &path)
                    .map(|right| media::visually_similar(left, &right))
                    .unwrap_or(true);
                (false, sim)
            } else {
                (false, true)
            }
        } else {
            match preview
                .as_ref()
                .ok_or_else(|| "Incoming preview is unavailable.".to_string())
                .and_then(|left| candidate_preview(&candidate, &path).map(|right| (left, right)))
            {
                Ok((left, right)) => {
                    let eq = auto && media::photo_equivalent(left, &right);
                    let sim = eq || media::visually_similar(left, &right);
                    (eq, sim)
                }
                Err(error) => {
                    if auto {
                        media.warnings.push(format!(
                            "Automatic comparison unavailable; original retained for review: {error}"
                        ));
                    }
                    (false, true)
                }
            }
        };
        if equivalent {
            matched = Some((candidate, dominates));
            continue;
        }
        if !plausible {
            continue;
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
    let path = archive.unique_import_path(&wanted, &id)?;
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
    if !is_lower && let Some(p) = preview {
        if candidate_cache.len() >= 128 {
            candidate_cache.clear();
        }
        candidate_cache.insert(id.clone(), p);
    }
    if let Some((old, dominates)) = matched
        && dominates
    {
        candidate_cache.remove(&old.id);
        archive.move_to_undo(&old.id, &id)?;
        archive.transfer_reviews(&old.id, &id)?;
        archive.release_import_path(Path::new(&old.path));
        if path != wanted && !paths::inside(&archive.root, &wanted)?.exists() {
            archive.relocate(&id, wanted.clone(), "active", None)?;
            archive.release_import_path(&path);
            archive.record_import_path(&wanted);
        }
    }
    if !is_lower {
        for (candidate, reason) in review {
            archive.add_review(&id, &candidate, &reason)?;
        }
    }
    Ok(false)
}
