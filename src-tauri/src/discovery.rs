use crate::{media, model::Result, paths};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use walkdir::WalkDir;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    path: String,
    count: usize,
    samples: Vec<String>,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub root: String,
    pub running: bool,
    pub cancelling: bool,
    pub cancelled: bool,
    pub current: String,
    pub directories: usize,
    pub files: usize,
    pub skipped: usize,
    pub issues: Vec<String>,
    pub excluded: usize,
    pub folders: Vec<Folder>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub status: Status,
    pub cancel: Arc<AtomicBool>,
}

pub fn scan(
    root: &str,
    archive: Option<&Path>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&Status) -> Result<()>,
) -> Result<Status> {
    let root = Path::new(root);
    let metadata =
        fs::symlink_metadata(root).map_err(|e| format!("Cannot open the selected folder: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Choose an existing folder that is not a shortcut or symlink.".into());
    }
    let root = paths::canonical(root)?;
    let archive = archive.map(paths::canonical).transpose()?;
    let archive = archive.as_deref();
    if archive.is_some_and(|archive| paths::starts_with(&root, archive)) {
        return Err("Choose a folder outside the current archive.".into());
    }
    let mut status = Status {
        root: root.to_string_lossy().into_owned(),
        running: true,
        ..Status::default()
    };
    let mut folders: HashMap<PathBuf, Folder> = HashMap::new();
    let mut updated = Instant::now();
    let entries = WalkDir::new(&root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter()
        .filter_entry(|entry| {
            entry.file_name() != ".singulo"
                && !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".singulo-cleanup-")
                && !archive.is_some_and(|archive| paths::starts_with(entry.path(), archive))
        });
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            status.cancelled = true;
            break;
        }
        match entry {
            Ok(entry) if entry.file_type().is_dir() => {
                status.directories += 1;
                status.current = entry.path().to_string_lossy().into_owned();
            }
            Ok(entry) if entry.file_type().is_file() => {
                if matches!(
                    media::kind(entry.path()),
                    Some("photo" | "heic" | "raw" | "video")
                ) {
                    status.files += 1;
                    if let Some(parent) = entry.path().parent() {
                        if archive.is_some_and(|archive| paths::starts_with(archive, parent)) {
                            status.excluded += 1;
                        } else {
                            let folder =
                                folders
                                    .entry(parent.to_path_buf())
                                    .or_insert_with(|| Folder {
                                        path: parent.to_string_lossy().into_owned(),
                                        count: 0,
                                        samples: Vec::new(),
                                    });
                            folder.count += 1;
                            if folder.samples.len() < 3 {
                                folder
                                    .samples
                                    .push(entry.file_name().to_string_lossy().into_owned());
                            }
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                status.skipped += 1;
                if status.issues.len() < 20 {
                    status.issues.push(error.to_string());
                }
            }
        }
        if updated.elapsed() >= Duration::from_millis(250) {
            progress(&status)?;
            updated = Instant::now();
        }
    }
    status.running = false;
    status.current.clear();
    status.folders = folders.into_values().collect();
    status
        .folders
        .sort_unstable_by(|a, b| b.count.cmp(&a.count).then_with(|| a.path.cmp(&b.path)));
    Ok(status)
}
