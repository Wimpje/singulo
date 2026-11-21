use crate::model::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const DIRECTORY: &str = "Singulo portable data";

#[derive(Serialize, Deserialize)]
struct ArchivePath {
    relative: bool,
    path: String,
    #[serde(default)]
    os: String,
    #[serde(default)]
    archive_id: String,
}

fn app_directory() -> Result<PathBuf> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut directory = exe
        .parent()
        .ok_or("Cannot find the app folder.")?
        .to_path_buf();
    #[cfg(target_os = "macos")]
    if directory.file_name().is_some_and(|name| name == "MacOS")
        && let Some(bundle) = directory.parent().and_then(Path::parent)
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app")
    {
        directory = bundle
            .parent()
            .ok_or("Cannot find the app folder.")?
            .to_path_buf();
    }
    Ok(directory)
}

fn data_directory() -> Result<Option<PathBuf>> {
    let path = app_directory()?.join(DIRECTORY);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err("Portable settings folder is not a safe directory.".into())
        }
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Cannot inspect portable settings: {error}")),
    }
}

fn database(directory: &Path) -> Result<Connection> {
    let path = directory.join("settings.sqlite");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Portable settings must not be a symlink.".into());
    }
    let db = Connection::open(path).map_err(|e| e.to_string())?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(|e| e.to_string())?;
    Ok(db)
}

fn relative_path(base: &Path, path: &Path) -> Option<PathBuf> {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = path.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let mut result = PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for component in &target[common..] {
        result.push(component.as_os_str());
    }
    Some(result)
}

fn archive_value(path: &Path) -> Result<String> {
    archive_value_at(&app_directory()?, path)
}

pub(crate) fn archive_value_at(base: &Path, path: &Path) -> Result<String> {
    let base = fs::canonicalize(base).map_err(|e| e.to_string())?;
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let archive_id = fs::read_to_string(path.join(".singulo/archive-id"))
        .map_err(|e| format!("Cannot read the archive identity: {e}"))?;
    if archive_id.trim().is_empty() || !path.join(".singulo/catalog.sqlite").is_file() {
        return Err("The selected archive is missing its identity or catalog.".into());
    }
    #[cfg(unix)]
    let same_disk = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&base).map_err(|e| e.to_string())?.dev()
            == fs::metadata(&path).map_err(|e| e.to_string())?.dev()
    };
    #[cfg(windows)]
    let same_disk = base.components().next().is_some_and(|component| {
        path.components().next().is_some_and(|other| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&other.as_os_str().to_string_lossy())
        })
    });
    #[cfg(not(any(unix, windows)))]
    let same_disk = false;
    let relative = same_disk.then(|| relative_path(&base, &path)).flatten();
    serde_json::to_string(&ArchivePath {
        relative: relative.is_some(),
        path: relative
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/"),
        os: std::env::consts::OS.into(),
        archive_id,
    })
    .map_err(|e| e.to_string())
}

pub fn enable(preferences: HashMap<String, String>, archive: Option<String>) -> Result<()> {
    let archive = archive
        .as_deref()
        .map(|path| archive_value(Path::new(path)))
        .transpose()?;
    let existing = data_directory()?;
    let directory = if let Some(directory) = &existing {
        directory.clone()
    } else {
        let directory =
            app_directory()?.join(format!("{DIRECTORY}.setup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)
            .map_err(|e| format!("Cannot create portable settings folder: {e}"))?;
        directory
    };
    let result = write_settings(&directory, preferences, archive);
    if let Err(error) = result {
        return if existing.is_some() {
            Err(error)
        } else {
            Err(format!(
                "Portable mode was not enabled: {error}. Incomplete setup remains at {}.",
                directory.display()
            ))
        };
    }
    if existing.is_none() {
        let destination = app_directory()?.join(DIRECTORY);
        if data_directory()?.is_some() {
            return Err(format!(
                "Portable settings appeared during setup. Incomplete setup remains at {}.",
                directory.display()
            ));
        }
        fs::rename(&directory, &destination).map_err(|e| {
            format!(
                "Cannot enable portable mode: {e}. Incomplete setup remains at {}.",
                directory.display()
            )
        })?;
    }
    Ok(())
}

fn write_settings(
    directory: &Path,
    preferences: HashMap<String, String>,
    archive: Option<String>,
) -> Result<()> {
    let mut db = database(directory)?;
    let transaction = db.transaction().map_err(|e| e.to_string())?;
    for (key, value) in preferences {
        if key.starts_with("singulo.") {
            transaction.execute(
                "INSERT INTO preferences VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, value],
            ).map_err(|e| e.to_string())?;
        }
    }
    if let Some(archive) = archive {
        transaction.execute(
            "INSERT INTO preferences VALUES('last-archive',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [archive],
        ).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())
}

pub fn preferences() -> Result<Option<HashMap<String, String>>> {
    let Some(directory) = data_directory()? else {
        return Ok(None);
    };
    let db = database(&directory)?;
    let mut query = db
        .prepare("SELECT key,value FROM preferences WHERE key LIKE 'singulo.%'")
        .map_err(|e| e.to_string())?;
    let values = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<std::result::Result<HashMap<_, _>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(Some(values))
}

pub fn set_preference(key: &str, value: &str) -> Result<()> {
    if !key.starts_with("singulo.") {
        return Err("Invalid portable preference.".into());
    }
    let directory = data_directory()?.ok_or("Portable settings folder is unavailable.")?;
    database(&directory)?.execute(
        "INSERT INTO preferences VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key,value],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_archive(path: &str) -> Result<bool> {
    let Some(directory) = data_directory()? else {
        return Ok(false);
    };
    database(&directory)?.execute(
        "INSERT INTO preferences VALUES('last-archive',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [archive_value(Path::new(path))?],
    ).map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn last_archive() -> Result<Option<Option<(String, String)>>> {
    let Some(directory) = data_directory()? else {
        return Ok(None);
    };
    let value: Option<String> = database(&directory)?
        .query_row(
            "SELECT value FROM preferences WHERE key='last-archive'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let archive = value
        .map(|value| resolve_archive_at(&app_directory()?, &value))
        .transpose()?;
    Ok(Some(archive))
}

pub(crate) fn resolve_archive_at(base: &Path, value: &str) -> Result<(String, String)> {
    let value: ArchivePath = serde_json::from_str(value).map_err(|e| e.to_string())?;
    if value.archive_id.trim().is_empty() {
        return Err(
            "The remembered archive has no saved identity. Choose the archive again.".into(),
        );
    }
    if !value.relative && value.os != std::env::consts::OS {
        return Err(
            "The previously selected archive is on another disk. Choose it on this computer."
                .into(),
        );
    }
    let path = if value.relative {
        base.join(value.path)
    } else {
        PathBuf::from(value.path)
    };
    Ok((path.to_string_lossy().into_owned(), value.archive_id))
}
