use serde::{Deserialize, Serialize};
use std::sync::{Mutex, atomic::AtomicBool};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub template: String,
    pub filenames: String,
    pub unknown_folder: String,
    pub automatic_matches: bool,
    #[serde(default)]
    pub deep_video_matching: bool,
    #[serde(default)]
    pub prevent_sleep_during_import: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            template: "{yyyy}/{mm}/{dd}".into(),
            filenames: "original".into(),
            unknown_folder: "unknown".into(),
            automatic_matches: true,
            deep_video_matching: false,
            prevent_sleep_during_import: false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub kind: String,
    pub date: Option<String>,
    pub date_source: String,
    pub warnings: Vec<String>,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    pub hash: String,
    pub bytes: u64,
    pub signature: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub path: String,
    pub original_name: String,
    pub state: String,
    pub keeper: Option<String>,
    pub media: Media,
    pub settings: Settings,
    pub albums: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    #[serde(default)]
    pub job_id: Option<String>,
    pub status: String,
    pub current: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub checked: usize,
    #[serde(default)]
    pub batch_files: usize,
    pub done: usize,
    pub total: usize,
    pub copied: usize,
    pub duplicates: usize,
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub folders: Vec<FolderProgress>,
    #[serde(default)]
    pub timings: ImportTimings,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderProgress {
    pub root: String,
    pub discovered: usize,
    pub processed: usize,
    pub copied: usize,
    pub duplicates: usize,
    pub failed: usize,
    pub scan_errors: usize,
    pub scan_complete: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTimings {
    pub scan_ms: u64,
    pub hash_ms: u64,
    pub metadata_ms: u64,
    pub buffer_ms: u64,
    pub decode_ms: u64,
    pub match_ms: u64,
    pub placement_ms: u64,
    pub backup_ms: u64,
    pub total_ms: u64,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            job_id: None,
            status: "idle".into(),
            current: String::new(),
            phase: String::new(),
            checked: 0,
            batch_files: 0,
            done: 0,
            total: 0,
            copied: 0,
            duplicates: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
            folders: Vec::new(),
            timings: ImportTimings::default(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub root: Option<String>,
    pub settings: Settings,
    pub progress: Progress,
    pub active_count: usize,
    pub undo_count: usize,
    pub active_bytes: u64,
    pub undo_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Computer {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub id: String,
    pub item: String,
    pub source: String,
    pub root: String,
    pub album: String,
    pub relative: String,
    pub hash: String,
    pub identity: String,
    pub status: String,
    pub bytes: u64,
    #[serde(default)]
    pub computer: Option<Computer>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupRow {
    pub id: String,
    pub source: String,
    pub root: String,
    pub bytes: u64,
    pub eligible: bool,
    pub reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearImportPreview {
    pub tracks_copies: bool,
    pub removable: usize,
    pub retained: usize,
    pub reasons: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveRow {
    pub id: String,
    pub from: String,
    pub to: String,
}

pub struct AppState {
    pub archive: Mutex<Option<crate::archive::Archive>>,
    pub archive_root: std::sync::RwLock<Option<std::path::PathBuf>>,
    pub progress: Mutex<Progress>,
    pub progress_updated: Mutex<std::time::Instant>,
    pub busy: AtomicBool,
    pub importing: AtomicBool,
    pub quit_after_import: AtomicBool,
    pub cancel: crate::cancel::Cancellation,
    pub discovery: Mutex<crate::discovery::State>,
    pub discovery_gate: Mutex<()>,
    pub pause: AtomicBool,
    pub queue: Mutex<std::collections::VecDeque<QueuedJob>>,
}

pub struct QueuedJob {
    pub id: String,
    pub roots: Vec<String>,
    pub settings: Settings,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            archive: Mutex::new(None),
            archive_root: std::sync::RwLock::new(None),
            progress: Mutex::new(Progress::default()),
            progress_updated: Mutex::new(std::time::Instant::now()),
            busy: AtomicBool::new(false),
            importing: AtomicBool::new(false),
            quit_after_import: AtomicBool::new(false),
            cancel: crate::cancel::Cancellation::default(),
            discovery: Mutex::new(crate::discovery::State::default()),
            discovery_gate: Mutex::new(()),
            pause: AtomicBool::new(false),
            queue: Mutex::new(std::collections::VecDeque::new()),
        }
    }
}
