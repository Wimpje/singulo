use crate::{archive::Archive, media, model::*, paths};
use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Mutex, atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

const BUFFER_LIMIT: u64 = 16 * 1024 * 1024;
const BUFFER_BUDGET: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct PreviewCache(VecDeque<(String, Rc<media::Preview>)>);

impl PreviewCache {
    fn get(&mut self, id: &str) -> Option<Rc<media::Preview>> {
        let index = self.0.iter().position(|(key, _)| key == id)?;
        let entry = self.0.remove(index)?;
        let preview = Rc::clone(&entry.1);
        self.0.push_back(entry);
        Some(preview)
    }

    fn insert(&mut self, id: String, preview: media::Preview) -> Rc<media::Preview> {
        self.remove(&id);
        if self.0.len() >= 128 {
            self.0.pop_front();
        }
        let preview = Rc::new(preview);
        self.0.push_back((id, Rc::clone(&preview)));
        preview
    }

    fn remove(&mut self, id: &str) {
        self.0.retain(|(key, _)| key != id);
    }
}

fn buffer_source(path: &Path, size: u64, identity: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(size as usize + 1);
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != size || media::identity(path)? != identity {
        return Err("Source changed while buffering; retry it.".into());
    }
    Ok(bytes)
}

pub fn update(app: &tauri::AppHandle, progress: &Progress) -> Result<()> {
    let state = app.state::<AppState>();
    let mut previous = state.progress.lock().map_err(|e| e.to_string())?;
    let mut updated = state.progress_updated.lock().map_err(|e| e.to_string())?;
    if previous.job_id == progress.job_id
        && previous.status == progress.status
        && previous.phase == progress.phase
        && updated.elapsed() < Duration::from_millis(250)
    {
        return Ok(());
    }
    *previous = progress.clone();
    *updated = Instant::now();
    app.emit("progress", progress).map_err(|e| e.to_string())
}

pub fn sync_xmp(state: &AppState) -> Result<Vec<String>> {
    let worker = {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let Some(archive) = archive.as_ref() else {
            return Ok(Vec::new());
        };
        archive.xmp_worker_if_needed()?
    };
    match worker {
        Some(worker) => Ok(worker.sync_exported_xmp()?.errors),
        None => Ok(Vec::new()),
    }
}

pub fn sync_xmp_warnings(state: &AppState, progress: &mut Progress) {
    match sync_xmp(state) {
        Ok(warnings) => progress.warnings.extend(
            warnings
                .into_iter()
                .map(|warning| format!("Managed XMP export needs attention: {warning}")),
        ),
        Err(error) => progress
            .warnings
            .push(format!("Managed XMP export needs attention: {error}")),
    }
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
    let started = Instant::now();
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
        job_id: Some(job.id.clone()),
        status: "scanning".into(),
        phase: "scanning".into(),
        ..Progress::default()
    };
    update(app, &progress)?;
    let roots = validate_roots(&job.roots, &archive_root)?;
    progress.folders = roots
        .iter()
        .map(|root| FolderProgress {
            root: root.to_string_lossy().into(),
            ..FolderProgress::default()
        })
        .collect();
    let mut files = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        if state.cancel.load(Ordering::Relaxed) {
            break;
        }
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
                    progress.current = entry.path().to_string_lossy().into();
                    files.push((root.clone(), entry.into_path()));
                    progress.folders[index].discovered += 1;
                }
                Ok(entry) => progress.current = entry.path().to_string_lossy().into(),
                Err(error) => {
                    progress.folders[index].scan_errors += 1;
                    progress.errors.push(format!("Folder scan: {error}"));
                }
            }
            progress.timings.total_ms = started.elapsed().as_millis() as u64;
            update(app, &progress)?;
        }
        progress.folders[index].scan_complete = !state.cancel.load(Ordering::Relaxed);
    }
    // Import image companions before XMP so sidecars can inherit their owner's date/path.
    files.sort_by_key(|(_, path)| media::kind(path) == Some("sidecar"));
    progress.total = files.len();
    progress.timings.scan_ms = started.elapsed().as_millis() as u64;
    progress.status = "importing".into();
    progress.phase = "hashing".into();
    update(app, &progress)?;
    let mut checkpoint = Instant::now();
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(1))
        .clamp(1, 8);
    let shared: &AppState = &state;
    let (task_tx, task_rx) = mpsc::sync_channel::<Task>(workers * 2);
    let task_rx = Mutex::new(task_rx);
    std::thread::scope(|scope| -> Result<()> {
        let (done_tx, done_rx) = mpsc::channel::<(usize, Inspected, u64)>();
        for _ in 0..workers {
            let (task_rx, done_tx) = (&task_rx, done_tx.clone());
            scope.spawn(move || {
                loop {
                    let task = match task_rx.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => break,
                    };
                    let Ok(task) = task else { break };
                    let decoding = Instant::now();
                    let result = if shared.cancel.load(Ordering::Relaxed) {
                        Err("Import cancelled.".into())
                    } else {
                        Ok(media::inspect(
                            app,
                            &task.path,
                            task.media,
                            Some(&task.bytes),
                        ))
                    };
                    if done_tx
                        .send((task.index, result, decoding.elapsed().as_millis() as u64))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(done_tx);
        let mut queue = VecDeque::<Entry>::new();
        let mut candidate_cache = PreviewCache::default();
        let mut base = 0;
        let receive = |queue: &mut VecDeque<Entry>, base: usize, (index, result, decode_ms)| {
            if let Some(entry) = queue.get_mut(index - base) {
                entry.stage = Stage::Inspected(Box::new(result));
                entry.decode_ms = decode_ms;
            }
        };
        let receive_waiting = |queue: &VecDeque<Entry>, progress: &mut Progress| {
            progress.phase = "decoding".into();
            if let Some(entry) = queue.front() {
                progress.current = entry.path.to_string_lossy().into();
            }
            progress.timings.total_ms = started.elapsed().as_millis() as u64;
            update(app, progress)?;
            done_rx
                .recv()
                .map_err(|_| "Decoder workers stopped unexpectedly.".to_string())
        };
        // Show the first results promptly, then amortize metadata startup across larger batches.
        let first_batch = files.len().min(4);
        for batch in files[..first_batch]
            .chunks(4)
            .chain(files[first_batch..].chunks(64))
        {
            if shared.cancel.load(Ordering::Relaxed) {
                break;
            }
            enum Prepped<'b> {
                Finished(&'b Path, &'b Path, Result<bool>),
                NeedsInspect(&'b Path, &'b Path, String, String),
            }
            let mut prepped = Vec::with_capacity(batch.len());
            let mut metadata_paths = Vec::new();
            let mut exact_occurrences = Vec::new();
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
                progress.phase = "hashing".into();
                progress.current = path.to_string_lossy().into();
                progress.timings.total_ms = started.elapsed().as_millis() as u64;
                update(app, &progress)?;
                let hashing = Instant::now();
                let checked: Result<Option<(String, String)>> = (|| {
                    let (identity, hash) = media::hash_with_identity(path)?;
                    let guard = shared.archive.lock().map_err(|e| e.to_string())?;
                    let archive = guard.as_ref().ok_or("Archive is not open.")?;
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
                        exact_occurrences.push(occ);
                        return Ok(None);
                    }
                    Ok(Some((identity, hash)))
                })();
                progress.timings.hash_ms += hashing.elapsed().as_millis() as u64;
                progress.checked += 1;
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
            if !exact_occurrences.is_empty() {
                let writing = Instant::now();
                let recorded: Result<()> = (|| {
                    let mut guard = shared.archive.lock().map_err(|e| e.to_string())?;
                    guard
                        .as_mut()
                        .ok_or("Archive is not open.")?
                        .record_import_occurrences(&job.id, &exact_occurrences)
                })();
                progress.timings.placement_ms += writing.elapsed().as_millis() as u64;
                if let Err(error) = recorded {
                    for item in &mut prepped {
                        if let Prepped::Finished(_, _, result @ Ok(true)) = item {
                            *result = Err(format!(
                                "Could not record duplicate source history: {error}"
                            ));
                        }
                    }
                }
            }
            let extracting = Instant::now();
            let metadata = if metadata_paths.is_empty() {
                Ok(HashMap::new())
            } else {
                progress.phase = "metadata".into();
                progress.batch_files = metadata_paths.len();
                progress.current.clear();
                progress.timings.total_ms = started.elapsed().as_millis() as u64;
                update(app, &progress)?;
                media::metadata_batch(app, &metadata_paths)
            };
            progress.timings.metadata_ms += extracting.elapsed().as_millis() as u64;
            for item in prepped {
                if shared.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match item {
                    Prepped::Finished(root, path, result) => Entry {
                        root,
                        path,
                        identity: String::new(),
                        stage: Stage::Finished(result),
                        buffered_bytes: 0,
                        decode_ms: 0,
                    },
                    Prepped::NeedsInspect(root, path, identity, hash) => {
                        progress.phase = if media::kind(path) == Some("raw") {
                            "raw"
                        } else {
                            "decoding"
                        }
                        .into();
                        progress.current = path.to_string_lossy().into();
                        progress.timings.total_ms = started.elapsed().as_millis() as u64;
                        update(app, &progress)?;
                        if media::identity(path).ok().as_ref() != Some(&identity) {
                            Entry {
                                root,
                                path,
                                identity: String::new(),
                                stage: Stage::Finished(Err(
                                    "Source changed during batch metadata extraction; retry it."
                                        .into(),
                                )),
                                buffered_bytes: 0,
                                decode_ms: 0,
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
                            let prepared = media::prepare_inspection(path, hash, row);
                            let mut buffered_bytes = 0;
                            let mut decode_ms = 0;
                            let stage = match prepared {
                                Err(error) => Stage::Finished(Err(error)),
                                Ok(media)
                                    if media.kind == "photo" && media.bytes <= BUFFER_LIMIT =>
                                {
                                    while queue
                                        .iter()
                                        .map(|entry| entry.buffered_bytes)
                                        .sum::<usize>()
                                        + media.bytes as usize
                                        > BUFFER_BUDGET
                                    {
                                        let done = receive_waiting(&queue, &mut progress)?;
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
                                    let reading = Instant::now();
                                    let bytes = buffer_source(path, media.bytes, &identity);
                                    progress.timings.buffer_ms +=
                                        reading.elapsed().as_millis() as u64;
                                    match bytes {
                                        Err(error) => Stage::Finished(Err(error)),
                                        Ok(bytes) => {
                                            buffered_bytes = bytes.len();
                                            let index = base + queue.len();
                                            task_tx
                                                .send(Task {
                                                    index,
                                                    path: path.to_path_buf(),
                                                    media,
                                                    bytes,
                                                })
                                                .map_err(
                                                    |_| "Decoder workers stopped unexpectedly.",
                                                )?;
                                            Stage::Inspecting
                                        }
                                    }
                                }
                                Ok(media) => {
                                    // External decoders and large images stay on the I/O coordinator.
                                    let decoding = Instant::now();
                                    let result = media::inspect(app, path, media, None);
                                    decode_ms = decoding.elapsed().as_millis() as u64;
                                    Stage::Inspected(Box::new(Ok(result)))
                                }
                            };
                            Entry {
                                root,
                                path,
                                identity,
                                stage,
                                buffered_bytes,
                                decode_ms,
                            }
                        }
                    }
                };
                queue.push_back(entry);
                progress.timings.total_ms = started.elapsed().as_millis() as u64;
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
                    let done = receive_waiting(&queue, &mut progress)?;
                    receive(&mut queue, base, done);
                }
            }
        }
        drop(task_tx);
        while !queue.is_empty() && !shared.cancel.load(Ordering::Relaxed) {
            let done = receive_waiting(&queue, &mut progress)?;
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
    progress.phase = "finalizing".into();
    progress.current.clear();
    progress.timings.total_ms = started.elapsed().as_millis() as u64;
    update(app, &progress)?;
    sync_xmp_warnings(&state, &mut progress);
    progress.timings.total_ms = started.elapsed().as_millis() as u64;
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
        let backing_up = Instant::now();
        if let Err(error) = archive.backup() {
            progress
                .errors
                .push(format!("Catalog recovery backup failed: {error}"));
            if progress.status == "complete" {
                progress.status = "complete with errors".into();
            }
        }
        progress.timings.backup_ms = backing_up.elapsed().as_millis() as u64;
        progress.timings.total_ms = started.elapsed().as_millis() as u64;
        archive.save_job(&job.id, &serde_json::json!({"id":job.id, "roots":job.roots, "settings":job.settings, "progress":progress}))?;
    }
    update(app, &progress)
}

type Inspected = Result<(Media, Option<media::Preview>)>;

struct Task {
    index: usize,
    path: PathBuf,
    media: Media,
    bytes: Vec<u8>,
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
    buffered_bytes: usize,
    decode_ms: u64,
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
    candidate_cache: &mut PreviewCache,
) -> Result<()> {
    let state = app.state::<AppState>();
    while queue
        .front()
        .is_some_and(|entry| !matches!(entry.stage, Stage::Inspecting))
    {
        let entry = queue.pop_front().ok_or("Import queue is empty.")?;
        progress.phase = "saving".into();
        progress.current = entry.path.to_string_lossy().into();
        update(app, progress)?;
        progress.timings.decode_ms += entry.decode_ms;
        *base += 1;
        let result = match entry.stage {
            Stage::Finished(result) => result,
            Stage::Inspected(inspected) => (*inspected).and_then(|inspected| {
                let mut guard = state.archive.lock().map_err(|e| e.to_string())?;
                let archive = guard.as_mut().ok_or("Archive is not open.")?;
                import_one(
                    app,
                    archive,
                    &job.id,
                    &job.settings,
                    entry.root,
                    entry.path,
                    &entry.identity,
                    inspected,
                    candidate_cache,
                    &mut progress.timings,
                )
            }),
            Stage::Inspecting => unreachable!(),
        };
        if let Some(folder) = progress
            .folders
            .iter_mut()
            .find(|folder| Path::new(&folder.root) == entry.root)
        {
            folder.processed += 1;
            match &result {
                Ok(true) => folder.duplicates += 1,
                Ok(false) => folder.copied += 1,
                Err(_) => folder.failed += 1,
            }
        }
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
    job_id: &str,
    settings: &Settings,
    root: &Path,
    source: &Path,
    identity: &str,
    (mut media, preview): (Media, Option<media::Preview>),
    candidate_cache: &mut PreviewCache,
    timings: &mut ImportTimings,
) -> Result<bool> {
    archive.check()?;
    let album = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let relative = source.strip_prefix(root).unwrap_or(source);
    let mut folder_parts = vec![album.clone()];
    if let Some(parent) = relative.parent() {
        folder_parts.extend(parent.components().filter_map(|part| match part {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
            _ => None,
        }));
    }
    let (folder_date, _) = media::folder_path_info(&folder_parts);
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
        archive.record_import_occurrences(job_id, &[occurrence(exact.id)])?;
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
    if media.date.is_none()
        && let Some(date) = folder_date
    {
        media.date = Some(date);
        media.date_source = "estimated: folder date".into();
        media
            .warnings
            .retain(|warning| warning != "No usable date. Set the date manually if known.");
    }
    let matching = Instant::now();
    let mut candidates = if media.kind == "sidecar" {
        Vec::new()
    } else {
        archive.candidates(&media)?
    };
    if media.kind == "video" {
        for candidate in archive.video_candidates(&media)? {
            if candidates.len() >= 10 {
                break;
            }
            if !candidates
                .iter()
                .any(|existing| existing.id == candidate.id)
            {
                candidates.push(candidate);
            }
        }
    }
    let mut matched: Option<(Item, bool)> = None;
    let mut review = Vec::new();
    for candidate in candidates {
        if media.kind == "video" && !(settings.deep_video_matching && settings.automatic_matches) {
            review.push((candidate.id,
                "Possible video match based on a preview or capture date and duration. Both originals are kept; compare before choosing.".into()));
            continue;
        }
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
        let auto = matched.is_none()
            && settings.automatic_matches
            && !protected
            && (dominates || dominated);
        let mut candidate_preview = |candidate: &Item, path: &Path| -> Result<Rc<media::Preview>> {
            if let Some(cached) = candidate_cache.get(&candidate.id) {
                return Ok(cached);
            }
            let decoded =
                media::preview(app, path, &candidate.media.kind, &candidate.media.metadata)?;
            Ok(candidate_cache.insert(candidate.id.clone(), decoded))
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
    timings.match_ms += matching.elapsed().as_millis() as u64;
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
        albums: vec![],
    };
    if matched.is_some() {
        item.media.warnings.push("Automatically grouped after full-frame equivalence checks; the higher-resolution copy is active. Original bytes remain in archive undo.".into());
    }
    let placing = Instant::now();
    archive.place_import(source, item, occurrence(id.clone()), job_id)?;
    if !is_lower && let Some(p) = preview {
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
    if !is_lower && !review.is_empty() {
        archive.transaction(|archive| {
            for (candidate, reason) in review {
                archive.add_review(&id, &candidate, &reason)?;
            }
            Ok(())
        })?;
    }
    timings.placement_ms += placing.elapsed().as_millis() as u64;
    Ok(false)
}
