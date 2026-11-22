mod archive;
mod cancel;
mod computer;
mod discovery;
mod import;
mod media;
mod model;
mod paths;
mod portable;
mod recycle;
mod sleep;
#[cfg(test)]
mod tests;

use archive::Archive;
use model::*;
use std::{fs, io::Cursor, path::Path, process::Command, sync::atomic::Ordering};
use tauri::{Emitter, Manager};

const SOURCE_CLEANUP_ENABLED: bool = false;

struct Busy<'a>(&'a AppState);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

fn guard(state: &AppState) -> Result<Busy<'_>> {
    state
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .map_err(|_| {
            "Another archive operation is running. Pause or finish it first.".to_string()
        })?;
    Ok(Busy(state))
}

async fn read<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&tauri::AppHandle) -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(move || action(&app))
        .await
        .map_err(|e| e.to_string())?
}

/// Imports hold the archive lock while placing each file. Rather than letting browsing wait for
/// that, contended reads use a short-lived read-only connection, which WAL keeps consistent.
fn with_catalog<T>(
    state: &AppState,
    action: impl FnOnce(Option<&Archive>) -> Result<T>,
) -> Result<T> {
    match state.archive.try_lock() {
        Ok(archive) => action(archive.as_ref()),
        Err(std::sync::TryLockError::WouldBlock) => {
            let root = state.archive_root.read().map_err(|e| e.to_string())?.clone();
            match root {
                Some(root) => action(Some(&Archive::open_reader(&root)?)),
                None => action(None),
            }
        }
        Err(std::sync::TryLockError::Poisoned(error)) => Err(error.to_string()),
    }
}

fn read_catalog<T>(state: &AppState, action: impl FnOnce(&Archive) -> Result<T>) -> Result<T> {
    with_catalog(state, |archive| {
        action(archive.ok_or("Choose an archive first.")?)
    })
}

async fn inspect_exclusive<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&Archive) -> Result<T> + Send + 'static,
) -> Result<T> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        action(archive.as_ref().ok_or("Choose an archive folder first.")?)
    })
    .await
}

async fn change<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&mut Archive) -> Result<T> + Send + 'static,
) -> Result<T> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        let (result, worker) = {
            let mut archive = state.archive.lock().map_err(|e| e.to_string())?;
            let archive = archive.as_mut().ok_or("Choose an archive folder first.")?;
            let result = action(archive);
            (result, archive.xmp_worker_if_needed())
        };
        let changed = result.is_ok();
        let sync = worker.and_then(|worker| match worker {
            Some(worker) => worker.sync_exported_xmp(),
            None => Ok(archive::XmpSummary::default()),
        });
        let attention = match sync {
            Ok(summary) => summary.errors,
            Err(error) => vec![format!("Managed XMP export failed: {error}")],
        };
        // Notification failures must not turn a committed action into a failed
        // command and prevent the caller's successful refresh/close behavior.
        if changed
            && let Err(error) = app.emit("library-changed", ())
        {
            eprintln!(
                "Archive changes were saved, but could not publish library-changed: {error}"
            );
        }
        if !attention.is_empty()
            && let Err(error) = app.emit(
                "xmp-attention",
                serde_json::json!({"errors": &attention}),
            )
        {
            eprintln!(
                "Could not publish xmp-attention: {error}. XMP report: {}",
                attention.join("; ")
            );
        }
        result
    })
    .await
}

fn current_snapshot(app: &tauri::AppHandle) -> Result<Snapshot> {
    let state = app.state::<AppState>();
    let progress = state.progress.lock().map_err(|e| e.to_string())?.clone();
    with_catalog(&state, |archive| {
        let Some(archive) = archive else {
            return Ok(Snapshot {
                root: None,
                settings: Settings::default(),
                progress,
                active_count: 0,
                undo_count: 0,
                active_bytes: 0,
                undo_bytes: 0,
            });
        };
        let (_, active_bytes, active_count) = archive.totals("active")?;
        let (undo_count, undo_bytes, _) = archive.totals("undo")?;
        Ok(Snapshot {
            root: Some(archive.root.to_string_lossy().into()),
            settings: archive.settings.clone(),
            progress,
            active_count,
            undo_count,
            active_bytes,
            undo_bytes,
        })
    })
}

#[tauri::command]
async fn snapshot(app: tauri::AppHandle) -> Result<Snapshot> {
    read(app, current_snapshot).await
}

#[tauri::command]
async fn start_folder_search(app: tauri::AppHandle, root: String) -> Result<()> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _gate = state.discovery_gate.lock().map_err(|e| e.to_string())?;
        if state.importing.load(Ordering::SeqCst) {
            return Err("Finish or cancel the import before searching for folders.".into());
        }
        if state.busy.load(Ordering::SeqCst) {
            return Err("Wait for the current archive operation before searching for folders.".into());
        }
        let archive = state.archive_root.read().map_err(|e| e.to_string())?.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut search = state.discovery.lock().map_err(|e| e.to_string())?;
            if search.status.running {
                return Err("A folder search is already running.".into());
            }
            search.status = discovery::Status {
                root: root.clone(),
                running: true,
                ..Default::default()
            };
            search.cancel = cancel.clone();
        }
        let handle = app.clone();
        let spawned = std::thread::Builder::new().name("folder-search".into()).spawn(move || {
            let outcome = discovery::scan(&root, archive.as_deref(), &cancel, |progress| {
                let state = handle.state::<AppState>();
                let mut search = state.discovery.lock().map_err(|e| e.to_string())?;
                search.status = progress.clone();
                search.status.cancelling = cancel.load(Ordering::Relaxed);
                Ok(())
            });
            let state = handle.state::<AppState>();
            match state.discovery.lock() {
                Ok(mut search) => match outcome {
                    Ok(status) => search.status = status,
                    Err(error) => {
                        search.status.running = false;
                        search.status.cancelling = false;
                        search.status.error = Some(error);
                    }
                },
                Err(error) => eprintln!("Cannot report folder search results: {error}"),
            };
        });
        if let Err(error) = spawned {
            state.discovery.lock().map_err(|e| e.to_string())?.status.running = false;
            return Err(format!("Cannot start folder search: {error}"));
        }
        Ok(())
    }).await
}

#[tauri::command]
async fn folder_search_status(app: tauri::AppHandle) -> Result<discovery::Status> {
    read(app, |app| {
        let state = app.state::<AppState>();
        let search = state.discovery.lock().map_err(|e| e.to_string())?;
        Ok(search.status.clone())
    }).await
}

#[tauri::command]
async fn cancel_folder_search(app: tauri::AppHandle) -> Result<()> {
    read(app, |app| {
        let state = app.state::<AppState>();
        let mut search = state.discovery.lock().map_err(|e| e.to_string())?;
        if search.status.running {
            search.cancel.store(true, Ordering::Relaxed);
            search.status.cancelling = true;
        }
        Ok(())
    }).await
}

#[tauri::command]
async fn open_archive(app: tauri::AppHandle, path: String, remembered: Option<bool>) -> Result<Snapshot> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _discovery_gate = state.discovery_gate.lock().map_err(|e| e.to_string())?;
        if state.discovery.lock().map_err(|e| e.to_string())?.status.running {
            return Err("Stop the folder search before changing the archive.".into());
        }
        let _busy = guard(&state)?;
        let root = {
            let mut slot = state.archive.lock().map_err(|e| e.to_string())?;
            if slot.as_ref().is_some_and(|a| {
                paths::canonical(Path::new(&path)).ok().as_ref() == Some(&a.root)
                    && a.check().is_ok()
            }) {
                drop(slot);
                return current_snapshot(app);
            }
            // Keep the fallback reader root in step with the slot while holding its lock.
            let mut reader_root = state.archive_root.write().map_err(|e| e.to_string())?;
            *slot = None;
            *reader_root = None;
            let expected = if remembered == Some(true) {
                match portable::last_archive()? {
                    Some(Some((saved_path, id))) if saved_path == path => Some(id),
                    Some(_) => return Err("The remembered archive changed. Choose it again.".into()),
                    None => None,
                }
            } else {
                None
            };
            let mut archive = if let Some(id) = expected {
                Archive::open_existing(Path::new(&path), &id)?
            } else {
                Archive::open(Path::new(&path))?
            };
            let errors = archive.recover()?;
            let root = archive.root.clone();
            *state.progress.lock().map_err(|e| e.to_string())? = Progress {
                errors,
                ..Progress::default()
            };
            *slot = Some(archive);
            *reader_root = Some(root.clone());
            root
        };
        let scope = app.asset_protocol_scope();
        let _ = scope.allow_directory(root.join(".singulo/thumbnails"), false);
        let _ = scope.allow_directory(root.join(".singulo/previews"), false);
        if !portable::set_archive(&path)? {
            let config = app.path().app_config_dir().map_err(|e| e.to_string())?;
            fs::create_dir_all(&config).map_err(|e| e.to_string())?;
            fs::write(config.join("last-archive.txt"), &path).map_err(|e| e.to_string())?;
        }
        app.emit("library-changed", ()).map_err(|e| e.to_string())?;
        current_snapshot(app)
    })
    .await
}

#[tauri::command]
async fn last_archive(app: tauri::AppHandle) -> Result<Option<String>> {
    read(app, |app| {
        if let Some(archive) = portable::last_archive()? {
            return Ok(archive.map(|(path, _)| path));
        }
        let path = app
            .path()
            .app_config_dir()
            .map_err(|e| e.to_string())?
            .join("last-archive.txt");
        match fs::read_to_string(path) {
            Ok(path) => Ok(Some(path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Cannot read last archive setting: {e}")),
        }
    })
    .await
}

#[tauri::command]
async fn portable_preferences(app: tauri::AppHandle) -> Result<Option<std::collections::HashMap<String, String>>> {
    read(app, |_| portable::preferences()).await
}

#[tauri::command]
async fn enable_portable(app: tauri::AppHandle, preferences: std::collections::HashMap<String, String>, archive: Option<String>) -> Result<()> {
    read(app, move |_| portable::enable(preferences, archive)).await
}

#[tauri::command]
async fn set_portable_preference(app: tauri::AppHandle, key: String, value: String) -> Result<()> {
    read(app, move |_| portable::set_preference(&key, &value)).await
}

#[tauri::command]
async fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<()> {
    change(app, move |archive| archive.save_settings(settings)).await
}

#[tauri::command]
async fn export_xmp(app: tauri::AppHandle) -> Result<archive::XmpSummary> {
    // Export has its own partial-success summary; do not turn it into a generic
    // change error or retry it implicitly after constructing the summary.
    run_xmp(app, false).await
}

#[tauri::command]
async fn remove_exported_xmp(app: tauri::AppHandle) -> Result<archive::XmpSummary> {
    run_xmp(app, true).await
}

async fn run_xmp(app: tauri::AppHandle, remove: bool) -> Result<archive::XmpSummary> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        let worker = {
            let archive = state.archive.lock().map_err(|e| e.to_string())?;
            archive
                .as_ref()
                .ok_or("Choose an archive folder first.")?
                .xmp_worker()?
        };
        if remove {
            worker.remove_exported_xmp()
        } else {
            worker.export_xmp()
        }
    })
    .await
}

#[tauri::command]
fn path_examples(settings: Settings) -> Result<Vec<String>> {
    paths::validate(&settings)?;
    [
        ("photo", Some("2023-07-18T14:32:10"), "IMG_1234.jpg"),
        ("video", Some("2023-07-18T14:32:10"), "clip.mov"),
        ("photo", Some("2023-07-18"), "scan.tif"),
        ("photo", None, "undated.jpg"),
        ("video", None, "undated.mov"),
    ]
    .into_iter()
    .map(|(kind, date, name)| {
        let media = Media {
            kind: kind.into(),
            date: date.map(str::to_string),
            date_source: "example".into(),
            warnings: vec![],
            width: 0,
            height: 0,
            duration_seconds: None,
            hash: String::new(),
            bytes: 0,
            signature: None,
            metadata: serde_json::json!({}),
        };
        paths::relative(&settings, &media, name).map(|p| p.to_string_lossy().replace('\\', "/"))
    })
    .collect()
}

#[tauri::command]
async fn library(
    app: tauri::AppHandle,
    search: String,
    filter: String,
    month: Option<String>,
    offset: usize,
) -> Result<Vec<Item>> {
    read(app, move |app| {
        read_catalog(&app.state::<AppState>(), |archive| {
            archive.page(&search, &filter, month.as_deref(), offset)
        })
    })
    .await
}

#[tauri::command]
async fn library_months(
    app: tauri::AppHandle,
    search: String,
    filter: String,
) -> Result<Vec<archive::LibraryMonth>> {
    read(app, move |app| {
        read_catalog(&app.state::<AppState>(), |archive| {
            archive.months(&search, &filter)
        })
    })
    .await
}

#[tauri::command]
async fn item_details(app: tauri::AppHandle, id: String) -> Result<serde_json::Value> {
    read(app, move |app| {
        read_catalog(&app.state::<AppState>(), |archive| {
            Ok(serde_json::json!({"item":archive.item(&id)?,
                "sources":archive.item_occurrences(&id)?,
                "companions":archive.companions(&id)?}))
        })
    })
    .await
}

#[tauri::command]
async fn preview_image(app: tauri::AppHandle, id: String, large: bool) -> Result<String> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let (root, item) = read_catalog(&state, |archive| {
            archive.check()?;
            Ok((archive.root.clone(), archive.item(&id)?))
        })?;
        if item.state == "purged" {
            return Err("This original was purged.".into());
        }
        let thumbnails = root.join(".singulo/thumbnails");
        let cached = thumbnails.join(format!(
            "{}-{}.jpg",
            item.id,
            if large { "large" } else { "small" }
        ));
        if !cached.is_file() {
            let path = paths::inside(&root, Path::new(&item.path))?;
            let size = if large { 2200 } else { 420 };
            let image =
                media::decode_sized(app, &path, &item.media.kind, &item.media.metadata, size)?;
            let mut data = Cursor::new(Vec::new());
            image
                .thumbnail(size, size)
                .to_rgb8()
                .write_to(&mut data, image::ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?;
            let pending = thumbnails.join(format!("{}.pending.jpg", uuid::Uuid::new_v4()));
            fs::write(&pending, data.into_inner()).map_err(|e| e.to_string())?;
            if let Err(error) = fs::rename(&pending, &cached) {
                let _ = fs::remove_file(&pending);
                if !cached.is_file() {
                    return Err(error.to_string());
                }
            }
        }
        app.asset_protocol_scope()
            .allow_file(&cached)
            .map_err(|e| e.to_string())?;
        Ok(cached.to_string_lossy().into())
    })
    .await
}

#[tauri::command]
async fn video_original(app: tauri::AppHandle, id: String) -> Result<String> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let (root, item) = read_catalog(&state, |archive| {
            archive.check()?;
            Ok((archive.root.clone(), archive.item(&id)?))
        })?;
        if item.media.kind != "video" || item.state == "purged" {
            return Err("This is not an available video.".into());
        }
        let path = paths::inside(&root, Path::new(&item.path))?;
        if !fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
            return Err("The original video is not a file.".into());
        }
        app.asset_protocol_scope()
            .allow_file(&path)
            .map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into())
    })
    .await
}

#[tauri::command]
async fn video_preview(app: tauri::AppHandle, id: String) -> Result<String> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let (root, item) = read_catalog(&state, |archive| {
            archive.check()?;
            Ok((archive.root.clone(), archive.item(&id)?))
        })?;
        if item.media.kind != "video" || item.state == "purged" {
            return Err("This is not an available video.".into());
        }
        let previews = root.join(".singulo/previews");
        let cache = previews.join(format!("{}.mp4", item.id));
        if !cache.exists() {
            let pending = previews.join(format!("{}.{}.pending.mp4", item.id, uuid::Uuid::new_v4()));
            let mut command = Command::new(media::tool(app, "ffmpeg")?);
            command
                .args(["-v", "error", "-nostdin", "-i"])
                .arg(paths::inside(&root, Path::new(&item.path))?)
                .args([
                    "-map",
                    "0:v:0",
                    "-map",
                    "0:a:0?",
                    "-vf",
                    "scale='min(1280,iw)':-2",
                    "-c:v",
                    if cfg!(target_os = "macos") {
                        "h264_videotoolbox"
                    } else {
                        "libopenh264"
                    },
                    "-b:v",
                    "4M",
                    "-c:a",
                    "aac",
                    "-movflags",
                    "+faststart",
                    "-y",
                ])
                .arg(&pending);
            if let Err(error) = media::execute(command, 3600) {
                let _ = fs::remove_file(&pending);
                return Err(error);
            }
            if let Err(error) = fs::rename(&pending, &cache) {
                let _ = fs::remove_file(&pending);
                if !cache.exists() {
                    return Err(error.to_string());
                }
            }
        }
        app.asset_protocol_scope()
            .allow_file(&cache)
            .map_err(|e| e.to_string())?;
        Ok(cache.to_string_lossy().into())
    })
    .await
}

#[tauri::command]
async fn start_import(app: tauri::AppHandle, roots: Vec<String>, deferred_only: Option<bool>) -> Result<bool> {
    if roots.is_empty() {
        return Err("Choose at least one source folder.".into());
    }
    let computer = computer::current()?;
    let deferred_only = deferred_only.unwrap_or(false);
    let state = app.state::<AppState>();
    let _discovery_gate = state.discovery_gate.lock().map_err(|e| e.to_string())?;
    if state.discovery.lock().map_err(|e| e.to_string())?.status.running {
        return Err("Stop the folder search before importing.".into());
    }
    if state.importing.load(Ordering::SeqCst) && state.cancel.requested() {
        return Err("Current import is cancelling; wait a moment before starting another.".into());
    }
    let (settings, validated_roots, deferred_roots) = {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let archive = archive
            .as_ref()
            .ok_or("Choose an archive folder before importing.")?;
        archive.check()?;
        let mut deferred_roots = std::collections::HashMap::new();
        let valid = if deferred_only {
            let deferred = archive.deferred_folders(&computer.id)?
                .into_iter().map(|folder| (folder.path.clone(), folder)).collect::<std::collections::HashMap<_, _>>();
            let mut original_roots = std::collections::HashMap::new();
            let mut seen = std::collections::HashSet::new();
            let mut valid = Vec::new();
            for root in &roots {
                let saved = deferred.get(root)
                    .ok_or("The selected deferred folder is no longer available.")?;
                let path = import::validate_roots(std::slice::from_ref(root), &archive.root)?
                    .into_iter().next().ok_or("Choose a source folder.")?;
                let path = path.to_string_lossy().into_owned();
                if path != *root {
                    return Err("A deferred folder changed location; rescan the source.".into());
                }
                if seen.insert(path.clone()) {
                    let original = match original_roots.entry(saved.root.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            let original = import::validate_roots(std::slice::from_ref(&saved.root), &archive.root)?
                                .into_iter().next().ok_or("The original source folder is unavailable.")?;
                            entry.insert(original)
                        }
                    };
                    if original.to_string_lossy() != saved.root || !paths::starts_with(Path::new(&path), original.as_path()) {
                        return Err("A deferred folder changed location; rescan the source.".into());
                    }
                    deferred_roots.insert(path.clone(), saved.root.clone());
                    valid.push(path);
                }
            }
            valid
        } else {
            import::validate_roots(&roots, &archive.root)?
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        if valid.is_empty() {
            return Err("Choose at least one source folder.".into());
        }
        (archive.settings.clone(), valid, deferred_roots)
    };
    let job = QueuedJob {
        id: uuid::Uuid::new_v4().to_string(),
        roots: validated_roots,
        settings,
        deferred_only,
        deferred_roots,
    };
    let mut queue = state.queue.lock().map_err(|e| e.to_string())?;
    let started = state
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok();
    if !started && !state.importing.load(Ordering::SeqCst) {
        return Err("Another archive operation is running; add folders after it finishes.".into());
    }
    if !started && state.cancel.requested() {
        return Err("Current import is cancelling; wait a moment before starting another.".into());
    }
    {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive.as_ref().ok_or("Archive is not open.")?.save_job(&job.id,
            &serde_json::json!({"id":job.id,"roots":job.roots,"settings":job.settings,"deferredOnly":job.deferred_only,"tracksCopies":true,"progress":{"status":"queued"}}))?;
    }
    queue.push_back(job);
    if started {
        state.cancel.reset();
        state.pause.store(false, Ordering::Relaxed);
        state.importing.store(true, Ordering::SeqCst);
    }
    drop(queue);
    if let Err(error) = app.emit("imports-changed", ()) {
        eprintln!("Could not publish import queue update: {error}");
    }
    if !started {
        return Ok(false);
    }
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        loop {
            let job = match state.queue.lock() {
                Ok(mut queue) => {
                    if state.cancel.requested() {
                        let drained: Vec<QueuedJob> = queue.drain(..).collect();
                        if let Ok(archive) = state.archive.lock()
                            && let Some(archive) = archive.as_ref()
                        {
                            for cancelled in drained {
                                let _ = archive.save_job(
                                    &cancelled.id,
                                    &serde_json::json!({
                                        "id": cancelled.id,
                                        "roots": cancelled.roots,
                                        "settings": cancelled.settings,
                                        "progress": { "status": "cancelled" }
                                    }),
                                );
                            }
                        }
                    }
                    let job = queue.pop_front();
                    if job.is_none() {
                        state.importing.store(false, Ordering::SeqCst);
                        state.busy.store(false, Ordering::SeqCst);
                    }
                    job
                }
                Err(error) => {
                    state.importing.store(false, Ordering::SeqCst);
                    state.busy.store(false, Ordering::SeqCst);
                    let progress = Progress {
                        status: "failed".into(),
                        errors: vec![format!("Import queue failed: {error}")],
                        ..Progress::default()
                    };
                    if let Err(error) = import::update(&handle, &progress) {
                        eprintln!("Could not publish queue failure: {error}");
                    }
                    break;
                }
            };
            let Some(job) = job else {
                break;
            };
            if let Err(error) = import::run(&handle, &job) {
                let mut progress = state
                    .progress
                    .lock()
                    .map(|progress| {
                        if progress.job_id.as_deref() == Some(job.id.as_str()) {
                            progress.clone()
                        } else {
                            Progress::default()
                        }
                    })
                    .unwrap_or_else(|lock_error| Progress {
                        errors: vec![format!(
                            "Could not read the last import progress: {lock_error}"
                        )],
                        ..Progress::default()
                    });
                progress.status = "failed".into();
                progress.job_id = Some(job.id.clone());
                progress.errors.push(error);
                import::sync_xmp_warnings(&state, &mut progress);
                let saved = (|| {
                    let archive = state.archive.lock().map_err(|e| e.to_string())?;
                    archive.as_ref().ok_or("Archive unavailable.")?.save_job(&job.id,
                        &serde_json::json!({"id":job.id,"roots":job.roots,"settings":job.settings,"progress":progress}))
                })();
                if let Err(error) = saved {
                    eprintln!("Could not persist failed import: {error}");
                }
                if let Err(error) = import::update(&handle, &progress) {
                    eprintln!("Could not publish import failure: {error}");
                }
            }
            if let Err(error) = handle.emit("imports-changed", ()) {
                eprintln!("Could not publish import history update: {error}");
            }
        }
        if let Err(error) = handle.emit("library-changed", ()) {
            eprintln!("Could not publish library update: {error}");
        }
        if state.quit_after_import.load(Ordering::SeqCst) {
            handle.exit(0);
        }
    });
    Ok(true)
}

#[tauri::command]
async fn deferred_folders(app: tauri::AppHandle) -> Result<Vec<DeferredFolder>> {
    read(app, |app| {
        let computer = computer::current()?;
        read_catalog(&app.state::<AppState>(), |archive| archive.deferred_folders(&computer.id))
    }).await
}

#[tauri::command]
async fn catalog_cleanup_preview(app: tauri::AppHandle) -> Result<Vec<CatalogCleanupFolder>> {
    inspect_exclusive(app, |archive| archive.catalog_cleanup_preview()).await
}

#[tauri::command]
async fn catalog_cleanup(
    app: tauri::AppHandle,
    folders: Vec<CatalogCleanupFolder>,
    confirmed: bool,
) -> Result<CatalogCleanupResult> {
    if !confirmed {
        return Err("Moving archive files to Trash requires explicit confirmation.".into());
    }
    change(app, move |archive| archive.catalog_cleanup(&folders)).await
}

#[tauri::command]
fn control_import(app: tauri::AppHandle, action: String) -> Result<()> {
    let state = app.state::<AppState>();
    let mut progress = state.progress.lock().map_err(|e| e.to_string())?;
    if !state.importing.load(Ordering::SeqCst) {
        return Err("No import is currently running.".into());
    }
    match action.as_str() {
        "pause" | "resume" if state.cancel.requested() => {}
        "pause" => state.pause.store(true, Ordering::Relaxed),
        "resume" => state.pause.store(false, Ordering::Relaxed),
        "cancel" => cancel_import(&app, &mut progress)?,
        _ => return Err("Unknown import control.".into()),
    }
    Ok(())
}

fn cancel_import(app: &tauri::AppHandle, progress: &mut Progress) -> Result<()> {
    let state = app.state::<AppState>();
    state.cancel.request();
    state.pause.store(false, Ordering::Relaxed);
    if import::mark_cancelling(progress) {
        app.emit("progress", &*progress).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Closing the last window stops a running import at a safe point and quits once it has
/// stopped; a later retry skips files that were already copied. Other operations still block.
fn close_requested(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    if window.app_handle().webview_windows().len() > 1 {
        return;
    }
    let state = window.state::<AppState>();
    // Set before checking `importing`; the import thread clears `importing` before reading this.
    state.quit_after_import.store(true, Ordering::SeqCst);
    if state.importing.load(Ordering::SeqCst) {
        api.prevent_close();
        let stopped = state
            .progress
            .lock()
            .map_err(|e| e.to_string())
            .and_then(|mut progress| cancel_import(window.app_handle(), &mut progress))
            .and_then(|()| {
                window
                    .emit("closing", ())
                    .map_err(|e| e.to_string())
            });
        if let Err(error) = stopped {
            eprintln!("Could not stop the import before closing: {error}");
        }
        return;
    }
    state.quit_after_import.store(false, Ordering::SeqCst);
    if state.busy.load(Ordering::SeqCst) {
        api.prevent_close();
        if let Err(error) = window.emit(
            "quit-blocked",
            "Finish or cancel the active operation before quitting.",
        ) {
            eprintln!("Could not report blocked quit: {error}");
        }
    }
}

#[tauri::command]
async fn job_history(app: tauri::AppHandle) -> Result<Vec<serde_json::Value>> {
    read(app, |app| {
        let state = app.state::<AppState>();
        let queued = state
            .queue
            .lock()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|job| job.id.clone())
            .collect::<Vec<_>>();
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let mut jobs = archive.as_ref().ok_or("Choose an archive first.")?.jobs()?;
        for job in &mut jobs {
            job["queued"] = serde_json::json!(
                job["id"].as_str().is_some_and(|id| queued.iter().any(|queued| queued == id))
            );
        }
        Ok(jobs)
    })
    .await
}

#[tauri::command]
async fn clear_import_preview(app: tauri::AppHandle, id: String) -> Result<ClearImportPreview> {
    inspect_exclusive(app, move |archive| {
        if SOURCE_CLEANUP_ENABLED {
            archive.clear_import_preview(&id)
        } else {
            archive.clear_import_preview_retaining_all(&id)
        }
    }).await
}

#[tauri::command]
async fn clear_import(
    app: tauri::AppHandle,
    id: String,
    trash_copies: bool,
    confirmed: bool,
) -> Result<()> {
    if !confirmed {
        return Err("Clearing an import requires explicit confirmation.".into());
    }
    if trash_copies {
        return Err("Removing archive copies during import cleanup is temporarily unavailable.".into());
    }
    let job = id.clone();
    change(app.clone(), move |archive| {
        archive.clear_import(&job, trash_copies)
    })
    .await?;
    let state = app.state::<AppState>();
    let mut progress = state.progress.lock().map_err(|e| e.to_string())?;
    if progress.job_id.as_deref() == Some(&id) {
        *progress = Progress::default();
        app.emit("progress", &*progress)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn review_list(app: tauri::AppHandle) -> Result<Vec<serde_json::Value>> {
    read(app, |app| {
        let state = app.state::<AppState>();
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive
            .as_ref()
            .ok_or("Choose an archive first.")?
            .reviews()
    })
    .await
}

#[tauri::command]
async fn review_groups(app: tauri::AppHandle, show_hidden: bool, offset: usize) -> Result<archive::ReviewGroupPage> {
    read(app, move |app| {
        read_catalog(&app.state::<AppState>(), |archive| archive.review_groups(show_hidden, offset))
    }).await
}

#[tauri::command]
async fn review_group_detail(app: tauri::AppHandle, show_hidden: bool, id: String, token: String, offset: usize) -> Result<archive::ReviewGroupDetail> {
    read(app, move |app| {
        read_catalog(&app.state::<AppState>(), |archive| archive.review_group_detail(show_hidden, &id, &token, offset))
    }).await
}

#[tauri::command]
async fn resolve_review_group(app: tauri::AppHandle, show_hidden: bool, id: String, token: String, discarded: Vec<String>) -> Result<archive::ReviewResult> {
    change(app, move |archive| archive.resolve_review_group(show_hidden, &id, &token, &discarded)).await
}

#[tauri::command]
async fn undo_review_group(app: tauri::AppHandle, token: String) -> Result<archive::ReviewResult> {
    change(app, move |archive| archive.undo_review_group(&token)).await
}

#[tauri::command]
async fn resolve_review(
    app: tauri::AppHandle,
    item: String,
    candidate: String,
    keeper: Option<String>,
) -> Result<()> {
    change(app, move |archive| {
        if let Some(keeper) = keeper {
            let discarded = if keeper == item {
                &candidate
            } else if keeper == candidate {
                &item
            } else {
                return Err("Choose one of the compared files as keeper.".into());
            };
            archive.move_to_undo(discarded, &keeper)?;
            archive.transfer_reviews(discarded, &keeper)?;
        }
        archive.dismiss_review(&item, &candidate)?;
        archive.backup()
    })
    .await
}

#[tauri::command]
async fn undo_review(
    app: tauri::AppHandle,
    item: String,
    candidate: String,
    reason: String,
    discarded: String,
) -> Result<()> {
    if discarded != item && discarded != candidate {
        return Err("Only a file from this comparison can be restored.".into());
    }
    change(app, move |archive| {
        // Re-add the review first; it stays hidden until both files are active again.
        archive.add_review(&item, &candidate, &reason)?;
        archive.restore(&discarded)
    })
    .await
}

#[tauri::command]
async fn restore_item(app: tauri::AppHandle, id: String) -> Result<()> {
    change(app, move |a| a.restore(&id)).await
}

#[tauri::command]
async fn reveal_item(app: tauri::AppHandle, id: String) -> Result<()> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let path = {
            let archive = state.archive.lock().map_err(|e| e.to_string())?;
            let archive = archive.as_ref().ok_or("Choose an archive first.")?;
            archive.check()?;
            let item = archive.item(&id)?;
            if item.state == "purged" {
                return Err("This original was purged.".into());
            }
            paths::inside(&archive.root, Path::new(&item.path))?
        };
        if !path.is_file() {
            return Err("This file can't be found. Is the archive disk connected?".into());
        }
        let mut child = reveal_command(&path)
            .spawn()
            .map_err(|e| format!("Could not show the file: {e}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    })
    .await
}

#[cfg(target_os = "macos")]
fn reveal_command(path: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg("-R").arg(path);
    command
}

#[cfg(windows)]
fn reveal_command(path: &Path) -> Command {
    use std::os::windows::process::CommandExt;
    let text = path.to_string_lossy();
    let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut command = Command::new("explorer");
    // Explorer needs the unsplit `/select,"path"` form; Windows paths cannot contain quotes.
    command.raw_arg(format!("/select,\"{plain}\""));
    command
}

#[cfg(not(any(target_os = "macos", windows)))]
fn reveal_command(path: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(path.parent().unwrap_or(path));
    command
}

#[tauri::command]
async fn purge_item(
    app: tauri::AppHandle,
    id: String,
    acknowledge_original_loss: bool,
) -> Result<()> {
    change(app, move |a| a.purge(&id, acknowledge_original_loss)).await
}

#[tauri::command]
async fn cleanup_preview(app: tauri::AppHandle) -> Result<Vec<CleanupRow>> {
    if !SOURCE_CLEANUP_ENABLED {
        return Err("Source cleanup is temporarily unavailable while computer-aware cleanup is being prepared.".into());
    }
    read(app, |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        let reader = {
            let archive = state.archive.lock().map_err(|e| e.to_string())?;
            archive.as_ref().ok_or("Choose an archive first.")?.cleanup_reader()?
        };
        reader.cleanup_rows()
    }).await
}

#[tauri::command]
async fn clean_sources(
    app: tauri::AppHandle,
    ids: Vec<String>,
    confirmed: bool,
    empty_folders: bool,
) -> Result<Vec<String>> {
    if !SOURCE_CLEANUP_ENABLED {
        return Err("Source cleanup is temporarily unavailable while computer-aware cleanup is being prepared.".into());
    }
    if !confirmed {
        return Err("Source cleanup requires explicit confirmation.".into());
    }
    change(app, move |archive| {
        let mut results = Vec::new();
        let mut roots = Vec::new();
        for id in ids {
            match archive.cleanup_one(&id) {
                Ok(root) => {
                    roots.push(root);
                    results.push("Moved verified source to OS trash.".into());
                }
                Err(error) => results.push(format!("Not cleaned: {error}")),
            }
        }
        if empty_folders {
            results.extend(archive.remove_empty_subfolders(&roots)?);
        }
        archive.backup()?;
        Ok(results)
    })
    .await
}

#[tauri::command]
async fn reorganization_preview(
    app: tauri::AppHandle,
    ids: Option<Vec<String>>,
) -> Result<Vec<MoveRow>> {
    let ids = ids.unwrap_or_default();
    inspect_exclusive(app, move |a| a.move_preview(&ids)).await
}

#[tauri::command]
async fn reorganize_archive(
    app: tauri::AppHandle,
    preview: Vec<MoveRow>,
    confirmed: bool,
) -> Result<()> {
    if !confirmed {
        return Err("Reorganization requires explicit confirmation.".into());
    }
    change(app, move |a| a.reorganize(&preview)).await
}

#[tauri::command]
async fn rollback_preview(app: tauri::AppHandle) -> Result<Vec<MoveRow>> {
    inspect_exclusive(app, |a| a.rollback_preview()).await
}

#[tauri::command]
async fn rollback_archive(
    app: tauri::AppHandle,
    preview: Vec<MoveRow>,
    confirmed: bool,
) -> Result<()> {
    if !confirmed {
        return Err("Undoing reorganization requires explicit confirmation.".into());
    }
    change(app, move |a| a.rollback(&preview)).await
}

#[tauri::command]
async fn set_date(app: tauri::AppHandle, id: String, date: String) -> Result<()> {
    change(app, move |a| a.correct_date(&id, &date)).await
}

#[tauri::command]
async fn repair_estimated_dates(
    app: tauri::AppHandle,
    confirmed: bool,
) -> Result<archive::DateRepairSummary> {
    if !confirmed {
        return Err("Correcting estimated dates requires explicit confirmation.".into());
    }
    change(app, |archive| archive.repair_estimated_dates()).await
}

#[tauri::command]
async fn tool_status(app: tauri::AppHandle) -> Result<Vec<serde_json::Value>> {
    read(app, |app| {
        Ok(
            ["exiftool", "ffmpeg", "ffprobe", "dcraw_emu", "heif-convert"]
                .into_iter()
                .map(|name| match media::tool(app, name) {
                    Ok(path) => serde_json::json!({"name":name,"available":true,"detail":path}),
                    Err(error) => serde_json::json!({"name":name,"available":false,"detail":error}),
                })
                .collect(),
        )
    })
    .await
}

// Must stay async: creating a window from a synchronous command deadlocks on Windows (wry#583).
#[tauri::command]
async fn show_window(app: tauri::AppHandle, kind: String) -> Result<()> {
    let window = match app.get_webview_window(&kind) {
        Some(window) => window,
        None => {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == kind)
                .ok_or("Unknown window.")?;
            tauri::WebviewWindowBuilder::from_config(&app, config)
                .and_then(|builder| builder.build())
                .map_err(|e| e.to_string())?
        }
    };
    window.show().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            start_folder_search,
            folder_search_status,
            cancel_folder_search,
            open_archive,
            last_archive,
            portable_preferences,
            enable_portable,
            set_portable_preference,
            save_settings,
            export_xmp,
            remove_exported_xmp,
            path_examples,
            library,
            library_months,
            item_details,
            preview_image,
            video_original,
            video_preview,
            start_import,
            deferred_folders,
            catalog_cleanup_preview,
            catalog_cleanup,
            control_import,
            job_history,
            clear_import_preview,
            clear_import,
            review_list,
            review_groups,
            review_group_detail,
            resolve_review_group,
            undo_review_group,
            resolve_review,
            undo_review,
            restore_item,
            reveal_item,
            purge_item,
            cleanup_preview,
            clean_sources,
            reorganization_preview,
            reorganize_archive,
            rollback_preview,
            rollback_archive,
            set_date,
            repair_estimated_dates,
            tool_status,
            show_window
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                close_requested(window, api);
            }
        })
        .run(tauri::generate_context!())
        .expect("Failed to start Singulo");
}
