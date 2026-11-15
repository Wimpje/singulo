mod archive;
mod import;
mod media;
mod model;
mod paths;
mod recycle;
#[cfg(test)]
mod tests;

use archive::Archive;
use base64::Engine;
use model::*;
use std::{fs, io::Cursor, path::Path, process::Command, sync::atomic::Ordering};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

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

async fn change<T: Send + 'static>(
    app: tauri::AppHandle,
    action: impl FnOnce(&mut Archive) -> Result<T> + Send + 'static,
) -> Result<T> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let result = {
            let _busy = guard(&state)?;
            let mut archive = state.archive.lock().map_err(|e| e.to_string())?;
            action(archive.as_mut().ok_or("Choose an archive folder first.")?)
        };
        app.emit("library-changed", ()).map_err(|e| e.to_string())?;
        result
    })
    .await
}

fn current_snapshot(app: &tauri::AppHandle) -> Result<Snapshot> {
    let state = app.state::<AppState>();
    let archive = state.archive.lock().map_err(|e| e.to_string())?;
    let progress = state.progress.lock().map_err(|e| e.to_string())?.clone();
    let Some(archive) = archive.as_ref() else {
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
    let items = archive.all()?;
    Ok(Snapshot {
        root: Some(archive.root.to_string_lossy().into()),
        settings: archive.settings.clone(),
        progress,
        active_count: items
            .iter()
            .filter(|i| i.state == "active" && i.media.kind != "sidecar")
            .count(),
        undo_count: items.iter().filter(|i| i.state == "undo").count(),
        active_bytes: items
            .iter()
            .filter(|i| i.state == "active")
            .map(|i| i.media.bytes)
            .sum(),
        undo_bytes: items
            .iter()
            .filter(|i| i.state == "undo")
            .map(|i| i.media.bytes)
            .sum(),
    })
}

#[tauri::command]
async fn snapshot(app: tauri::AppHandle) -> Result<Snapshot> {
    read(app, current_snapshot).await
}

#[tauri::command]
async fn open_archive(app: tauri::AppHandle, path: String) -> Result<Snapshot> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        {
            let mut slot = state.archive.lock().map_err(|e| e.to_string())?;
            if slot
                .as_ref()
                .is_some_and(|a| a.root == Path::new(&path).canonicalize().unwrap_or_default())
            {
                drop(slot);
                return current_snapshot(app);
            }
            let mut archive = Archive::open(Path::new(&path))?;
            let errors = archive.recover()?;
            *state.progress.lock().map_err(|e| e.to_string())? = Progress {
                errors,
                ..Progress::default()
            };
            *slot = Some(archive);
        }
        let config = app.path().app_config_dir().map_err(|e| e.to_string())?;
        fs::create_dir_all(&config).map_err(|e| e.to_string())?;
        fs::write(config.join("last-archive.txt"), &path).map_err(|e| e.to_string())?;
        app.emit("library-changed", ()).map_err(|e| e.to_string())?;
        current_snapshot(app)
    })
    .await
}

#[tauri::command]
async fn last_archive(app: tauri::AppHandle) -> Result<Option<String>> {
    read(app, |app| {
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
async fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<()> {
    change(app, move |archive| archive.save_settings(settings)).await
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
    offset: usize,
) -> Result<Vec<Item>> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let archive = archive.as_ref().ok_or("Choose an archive first.")?;
        let search = search.to_lowercase();
        let mut items: Vec<_> = archive
            .all()?
            .into_iter()
            .filter(|item| {
                let visible = match filter.as_str() {
                    "undo" => item.state == "undo",
                    "unknown" => item.state == "active" && item.media.date.is_none(),
                    "estimated" => {
                        item.state == "active" && item.media.date_source.starts_with("estimated")
                    }
                    "video" => item.state == "active" && item.media.kind == "video",
                    _ => item.state == "active",
                };
                visible
                    && item.media.kind != "sidecar"
                    && (search.is_empty()
                        || item.original_name.to_lowercase().contains(&search)
                        || item
                            .media
                            .date
                            .as_ref()
                            .is_some_and(|d| d.contains(&search))
                        || item
                            .albums
                            .iter()
                            .any(|a| a.to_lowercase().contains(&search)))
            })
            .collect();
        items.sort_by(|a, b| {
            b.media
                .date
                .cmp(&a.media.date)
                .then_with(|| a.original_name.cmp(&b.original_name))
        });
        Ok(items.into_iter().skip(offset).take(60).collect())
    })
    .await
}

#[tauri::command]
async fn item_details(app: tauri::AppHandle, id: String) -> Result<serde_json::Value> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let archive = archive.as_ref().ok_or("Choose an archive first.")?;
        Ok(serde_json::json!({"item":archive.item(&id)?,
            "sources":archive.occurrences()?.into_iter().filter(|o| o.item == id).collect::<Vec<_>>(),
            "companions":archive.companions(&id)?}))
    }).await
}

#[tauri::command]
async fn preview_image(app: tauri::AppHandle, id: String, large: bool) -> Result<String> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let (root, item) = {
            let archive = state.archive.lock().map_err(|e| e.to_string())?;
            let archive = archive.as_ref().ok_or("Choose an archive first.")?;
            archive.check()?;
            (archive.root.clone(), archive.item(&id)?)
        };
        if item.state == "purged" {
            return Err("This original was purged.".into());
        }
        let cached = root.join(".photo-dedupe/thumbnails").join(format!(
            "{}-{}.jpg",
            item.id,
            if large { "large" } else { "small" }
        ));
        let bytes = if cached.is_file() {
            fs::read(cached).map_err(|e| e.to_string())?
        } else {
            let path = paths::inside(&root, Path::new(&item.path))?;
            let image = media::decode(app, &path, &item.media.kind, &item.media.metadata)?;
            let size = if large { 2200 } else { 420 };
            let mut data = Cursor::new(Vec::new());
            image
                .thumbnail(size, size)
                .to_rgb8()
                .write_to(&mut data, image::ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?;
            let bytes = data.into_inner();
            fs::write(cached, &bytes).map_err(|e| e.to_string())?;
            bytes
        };
        Ok(format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
    .await
}

#[tauri::command]
async fn video_preview(app: tauri::AppHandle, id: String) -> Result<String> {
    read(app, move |app| {
        let state = app.state::<AppState>();
        let _busy = guard(&state)?;
        let (root, item) = {
            let archive = state.archive.lock().map_err(|e| e.to_string())?;
            let archive = archive.as_ref().ok_or("Choose an archive first.")?;
            archive.check()?;
            (archive.root.clone(), archive.item(&id)?)
        };
        if item.media.kind != "video" || item.state == "purged" {
            return Err("This is not an available video.".into());
        }
        let cache = root
            .join(".photo-dedupe/previews")
            .join(format!("{}.mp4", item.id));
        if !cache.exists() {
            let pending = root
                .join(".photo-dedupe/previews")
                .join(format!("{}.pending.mp4", item.id));
            if pending.exists() {
                fs::remove_file(&pending).map_err(|e| e.to_string())?;
            }
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
            media::execute(command, 3600)?;
            fs::rename(pending, &cache).map_err(|e| e.to_string())?;
        }
        app.asset_protocol_scope()
            .allow_file(&cache)
            .map_err(|e| e.to_string())?;
        Ok(cache.to_string_lossy().into())
    })
    .await
}

#[tauri::command]
async fn start_import(app: tauri::AppHandle, roots: Vec<String>) -> Result<bool> {
    if roots.is_empty() {
        return Err("Choose at least one source folder.".into());
    }
    let state = app.state::<AppState>();
    let settings = {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        let archive = archive
            .as_ref()
            .ok_or("Choose an archive folder before importing.")?;
        archive.check()?;
        archive.settings.clone()
    };
    let job = QueuedJob {
        id: uuid::Uuid::new_v4().to_string(),
        roots,
        settings,
    };
    {
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive.as_ref().ok_or("Archive is not open.")?.save_job(&job.id,
            &serde_json::json!({"id":job.id,"roots":job.roots,"settings":job.settings,"progress":{"status":"queued"}}))?;
    }
    let mut queue = state.queue.lock().map_err(|e| e.to_string())?;
    let started = state
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok();
    if !started && !state.importing.load(Ordering::SeqCst) {
        return Err("Another archive operation is running; add folders after it finishes.".into());
    }
    queue.push_back(job);
    if !started {
        return Ok(false);
    }
    state.importing.store(true, Ordering::SeqCst);
    state.cancel.store(false, Ordering::Relaxed);
    state.pause.store(false, Ordering::Relaxed);
    drop(queue);
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = handle.state::<AppState>();
        loop {
            let job = match state.queue.lock() {
                Ok(mut queue) => {
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
                    if let Err(error) = import::update(&handle, progress) {
                        eprintln!("Could not publish queue failure: {error}");
                    }
                    break;
                }
            };
            let Some(job) = job else {
                break;
            };
            if let Err(error) = import::run(&handle, &job) {
                let progress = Progress {
                    status: "failed".into(),
                    errors: vec![error],
                    ..Progress::default()
                };
                let saved = (|| {
                    let archive = state.archive.lock().map_err(|e| e.to_string())?;
                    archive.as_ref().ok_or("Archive unavailable.")?.save_job(&job.id,
                        &serde_json::json!({"id":job.id,"roots":job.roots,"settings":job.settings,"progress":progress}))
                })();
                if let Err(error) = saved {
                    eprintln!("Could not persist failed import: {error}");
                }
                if let Err(error) = import::update(&handle, progress) {
                    eprintln!("Could not publish import failure: {error}");
                }
            }
            if state.cancel.load(Ordering::Relaxed) {
                match state.queue.lock() {
                    Ok(mut queue) => {
                        queue.clear();
                        state.importing.store(false, Ordering::SeqCst);
                        state.busy.store(false, Ordering::SeqCst);
                    }
                    Err(error) => {
                        state.importing.store(false, Ordering::SeqCst);
                        state.busy.store(false, Ordering::SeqCst);
                        eprintln!("Could not cancel queued imports: {error}");
                    }
                }
                break;
            }
        }
        if let Err(error) = handle.emit("library-changed", ()) {
            eprintln!("Could not publish library update: {error}");
        }
    });
    Ok(true)
}

#[tauri::command]
fn control_import(app: tauri::AppHandle, action: String) -> Result<()> {
    let state = app.state::<AppState>();
    if !state.importing.load(Ordering::SeqCst) {
        return Err("No import is currently running.".into());
    }
    match action.as_str() {
        "pause" => state.pause.store(true, Ordering::Relaxed),
        "resume" => state.pause.store(false, Ordering::Relaxed),
        "cancel" => {
            state.cancel.store(true, Ordering::Relaxed);
            state.pause.store(false, Ordering::Relaxed);
        }
        _ => return Err("Unknown import control.".into()),
    }
    Ok(())
}

#[tauri::command]
async fn job_history(app: tauri::AppHandle) -> Result<Vec<serde_json::Value>> {
    read(app, |app| {
        let state = app.state::<AppState>();
        let archive = state.archive.lock().map_err(|e| e.to_string())?;
        archive.as_ref().ok_or("Choose an archive first.")?.jobs()
    })
    .await
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
        }
        archive.dismiss_review(&item, &candidate)?;
        archive.backup()
    })
    .await
}

#[tauri::command]
async fn restore_item(app: tauri::AppHandle, id: String) -> Result<()> {
    change(app, move |a| a.restore(&id)).await
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
    change(app, |a| a.cleanup_rows()).await
}

#[tauri::command]
async fn clean_sources(
    app: tauri::AppHandle,
    ids: Vec<String>,
    confirmed: bool,
    empty_folders: bool,
) -> Result<Vec<String>> {
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
async fn reorganization_preview(app: tauri::AppHandle, ids: Vec<String>) -> Result<Vec<MoveRow>> {
    change(app, move |a| a.move_preview(&ids)).await
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
    change(app, |a| a.rollback_preview()).await
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

#[tauri::command]
fn show_window(app: tauri::AppHandle, kind: String) -> Result<()> {
    if !["main", "drop", "viewer"].contains(&kind.as_str()) {
        return Err("Unknown window.".into());
    }
    if let Some(window) = app.get_webview_window(&kind) {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let builder = WebviewWindowBuilder::new(
        &app,
        &kind,
        WebviewUrl::App(format!("/?window={kind}").into()),
    )
    .title(if kind == "drop" {
        "Drop into Family Archive"
    } else {
        "Family Archive Viewer"
    })
    .inner_size(
        if kind == "drop" { 310.0 } else { 1100.0 },
        if kind == "drop" { 260.0 } else { 800.0 },
    )
    .always_on_top(kind == "drop");
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            open_archive,
            last_archive,
            save_settings,
            path_examples,
            library,
            item_details,
            preview_image,
            video_preview,
            start_import,
            control_import,
            job_history,
            review_list,
            resolve_review,
            restore_item,
            purge_item,
            cleanup_preview,
            clean_sources,
            reorganization_preview,
            reorganize_archive,
            rollback_preview,
            rollback_archive,
            set_date,
            tool_status,
            show_window
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.state::<AppState>().busy.load(Ordering::SeqCst)
            {
                api.prevent_close();
                if let Err(error) = window.emit(
                    "quit-blocked",
                    "Finish or cancel the active operation before quitting.",
                ) {
                    eprintln!("Could not report blocked quit: {error}");
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Failed to start Family Archive");
}
