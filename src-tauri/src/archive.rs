use crate::{media, model::*, paths};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

mod xmp;
pub use xmp::XmpSummary;

#[derive(Serialize, Deserialize)]
struct Operation {
    item: Item,
    occurrence: Option<Occurrence>,
    stage: String,
    old_path: Option<String>,
    #[serde(default)]
    import_job: Option<String>,
}

#[derive(Serialize)]
pub struct LibraryMonth {
    month: String,
    count: i64,
}

const LIBRARY_SEARCH: &str =
    "AND (?2='' OR instr(unicode_lower(json_extract(data,'$.originalName')),?2)>0
    OR instr(json_extract(data,'$.media.date'),?2)>0
    OR EXISTS (SELECT 1 FROM json_each(data,'$.albums') WHERE instr(unicode_lower(value),?2)>0))";

fn library_filter(filter: &str) -> (&str, &str) {
    let state = if filter == "undo" { "undo" } else { "active" };
    let condition = match filter {
        "unknown" => "AND json_extract(data,'$.media.date') IS NULL",
        "estimated" => "AND json_extract(data,'$.media.dateSource') LIKE 'estimated%'",
        "video" => "AND json_extract(data,'$.media.kind')='video'",
        _ => "",
    };
    (state, condition)
}

#[cfg(test)]
mod library_tests {
    use super::*;

    #[test]
    fn review_list_contains_thumbnails_for_both_active_items() {
        let directory = tempfile::tempdir().unwrap();
        let mut archive = Archive::open(directory.path()).unwrap();
        for (id, name, kind, state) in [
            ("first", "first.jpg", "photo", "active"),
            ("second", "second.mov", "video", "active"),
            ("undo", "undo.jpg", "photo", "undo"),
        ] {
            let data = serde_json::json!({
                "originalName": name,
                "media": {"kind": kind, "bytes": 1}
            });
            archive.connection.execute(
                "INSERT INTO items(id,hash,state,data) VALUES (?1,?1,?2,?3)",
                params![id, state, data.to_string()],
            ).unwrap();
        }
        archive.add_review("first", "second", "Similar").unwrap();
        archive.add_review("first", "undo", "Inactive").unwrap();
        let reviews = archive.reviews().unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0]["item"], "first");
        assert_eq!(reviews[0]["candidate"], "second");
        assert_eq!(reviews[0]["reason"], "Similar");
        assert_eq!(reviews[0]["itemPreview"], serde_json::json!({
            "id": "first", "originalName": "first.jpg", "media": {"kind": "photo"}
        }));
        assert_eq!(reviews[0]["candidatePreview"], serde_json::json!({
            "id": "second", "originalName": "second.mov", "media": {"kind": "video"}
        }));
    }

    #[test]
    fn months_and_pages_respect_filters_and_keep_ties_stable() {
        let directory = tempfile::tempdir().unwrap();
        let archive = Archive::open(directory.path()).unwrap();
        let mut item = Item {
            id: String::new(),
            path: "photo.jpg".into(),
            original_name: "same.jpg".into(),
            state: "active".into(),
            keeper: None,
            media: Media {
                kind: "photo".into(),
                date: Some("2023-07-18T14:32:10".into()),
                date_source: "capture metadata".into(),
                warnings: vec![],
                width: 1,
                height: 1,
                duration_seconds: None,
                hash: "hash".into(),
                bytes: 1,
                signature: None,
                metadata: serde_json::json!({}),
            },
            settings: Settings::default(),
            albums: vec!["Holiday".into()],
        };
        let insert = |item: &Item| {
            archive
                .connection
                .execute(
                    "INSERT INTO items(id,hash,state,data) VALUES (?1,?2,?3,?4)",
                    params![item.id, item.media.hash, item.state, json(item).unwrap()],
                )
                .unwrap();
        };
        for index in 0..65 {
            item.id = format!("photo-{index:03}");
            insert(&item);
        }
        item.id = "june-video".into();
        item.media.date = Some("2023-06-02".into());
        item.media.kind = "video".into();
        insert(&item);
        item.id = "undated".into();
        item.media.date = None;
        item.media.kind = "photo".into();
        insert(&item);
        item.id = "sidecar".into();
        item.media.kind = "sidecar".into();
        insert(&item);
        item.id = "undo".into();
        item.state = "undo".into();
        item.media.kind = "photo".into();
        insert(&item);

        let months = archive.months("", "all").unwrap();
        assert_eq!(
            months
                .iter()
                .map(|m| (m.month.as_str(), m.count))
                .collect::<Vec<_>>(),
            [("2023-07", 65), ("2023-06", 1), ("unknown", 1)]
        );
        assert_eq!(
            archive.months("holiday", "video").unwrap()[0].month,
            "2023-06"
        );
        assert!(archive.months("missing", "all").unwrap().is_empty());
        assert_eq!(archive.months("", "undo").unwrap()[0].count, 1);
        assert_eq!(
            archive
                .page("", "unknown", Some("unknown"), 0)
                .unwrap()
                .len(),
            1
        );
        let first = archive.page("", "all", Some("2023-07"), 0).unwrap();
        let second = archive.page("", "all", Some("2023-07"), 60).unwrap();
        assert_eq!(first.len(), 60);
        assert_eq!(second.len(), 5);
        assert_eq!(first[59].id, "photo-059");
        assert_eq!(second[0].id, "photo-060");
        assert!(
            archive
                .page("", "all", Some("2023-07"), 120)
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod performance_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "Explicit 100k-record scale check; not part of the basic test suite"]
    fn catalog_handles_100k_records() {
        let directory = tempfile::tempdir().unwrap();
        let mut archive = Archive::open(directory.path()).unwrap();
        let item = Item {
            id: "seed".into(),
            path: "2023/07/18/photo.jpg".into(),
            original_name: "photo.jpg".into(),
            state: "active".into(),
            keeper: None,
            media: Media {
                kind: "photo".into(),
                date: Some("2023-07-18T14:32:10".into()),
                date_source: "capture metadata".into(),
                warnings: vec![],
                width: 4000,
                height: 3000,
                duration_seconds: None,
                hash: "seed".into(),
                bytes: 4_000_000,
                signature: None,
                metadata: serde_json::json!({"EXIF:Orientation":1}),
            },
            settings: Settings::default(),
            albums: vec!["Holiday".into()],
        };
        let occurrence = Occurrence {
            id: "seed".into(),
            item: "seed".into(),
            source: "/fixture/photo.jpg".into(),
            root: "/fixture".into(),
            album: "Holiday".into(),
            relative: "photo.jpg".into(),
            hash: "seed".into(),
            identity: "fixture".into(),
            status: "pending".into(),
            bytes: 4_000_000,
        };
        let start = Instant::now();
        archive.transaction(|archive| {
            archive.connection.execute(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000)
                 INSERT INTO items SELECT printf('photo-%06d',x),printf('hash-%06d',x),'active',
                 json_set(?1,'$.id',printf('photo-%06d',x),'$.media.hash',printf('hash-%06d',x),
                     '$.originalName',printf('photo-%06d.jpg',x)) FROM n",
                [json(&item)?]).map_err(|e| e.to_string())?;
            archive.connection.execute(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000)
                 INSERT INTO occurrences SELECT printf('source-%06d',x),printf('/fixture/photo-%06d.jpg',x),printf('hash-%06d',x),
                 json_set(?1,'$.id',printf('source-%06d',x),'$.item',printf('photo-%06d',x),
                     '$.source',printf('/fixture/photo-%06d.jpg',x),'$.hash',printf('hash-%06d',x)) FROM n",
                [json(&occurrence)?]).map_err(|e| e.to_string())?;
            Ok(())
        }).unwrap();
        let seed_time = start.elapsed();
        // Exercise migration of a pre-existing large catalog, not just a new one.
        archive
            .connection
            .execute_batch("PRAGMA user_version=1")
            .unwrap();
        drop(archive);
        let start = Instant::now();
        let mut archive = Archive::open(directory.path()).unwrap();
        let migration_time = start.elapsed();
        assert_eq!(
            archive.totals("active").unwrap(),
            (100_000, 400_000_000_000, 100_000)
        );
        let start = Instant::now();
        let page = archive.page("", "all", None, 99_900).unwrap();
        assert_eq!(page.len(), 60);
        assert_eq!(page[0].original_name, "photo-099901.jpg");
        let page_time = start.elapsed();
        assert_eq!(
            archive.page("photo-000001", "all", None, 0).unwrap().len(),
            1
        );
        let start = Instant::now();
        let months = archive.months("", "all").unwrap();
        assert_eq!((months[0].month.as_str(), months[0].count), ("2023-07", 100_000));
        let months_time = start.elapsed();
        let start = Instant::now();
        let month_page = archive.page("", "all", Some("2023-07"), 99_900).unwrap();
        assert_eq!(month_page.len(), 60);
        assert_eq!(month_page[0].original_name, "photo-099901.jpg");
        let month_page_time = start.elapsed();
        eprintln!("100k month grouping: {months_time:?}; deep month page: {month_page_time:?}");
        let mut sidecar = occurrence;
        sidecar.id = "xmp".into();
        sidecar.source = "/fixture/photo-000001.xmp".into();
        sidecar.item = "photo-000002".into();
        sidecar.hash = "xmp".into();
        let start = Instant::now();
        archive.record_occurrence(&sidecar).unwrap();
        let link_time = start.elapsed();
        assert!(
            archive
                .companions("photo-000001")
                .unwrap()
                .contains(&sidecar.item)
        );
        assert_eq!(
            archive
                .occurrence_id(Path::new(&sidecar.source), "xmp")
                .unwrap()
                .as_deref(),
            Some("xmp")
        );
        assert_eq!(archive.item_occurrences(&sidecar.item).unwrap().len(), 2);
        assert_eq!(archive.totals("active").unwrap().0, 100_000);
        let plan: String = archive
            .connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT item FROM source_groups WHERE group_key=?1",
                ["/fixture/photo-000001"],
                |row| row.get(3),
            )
            .unwrap();
        assert!(plan.contains("source_group_key"), "{plan}");
        let start = Instant::now();
        archive.backup().unwrap();
        let backup_time = start.elapsed();
        let export: serde_json::Value = serde_json::from_reader(
            File::open(
                directory
                    .path()
                    .join(".singulo/recovery/metadata-latest.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(export["items"].as_array().unwrap().len(), 100_000);
        assert_eq!(export["occurrences"].as_array().unwrap().len(), 100_001);
        println!(
            "100k catalog: seed={seed_time:?}, migration={migration_time:?}, deep page={page_time:?}, indexed companion link={link_time:?}, streamed backup={backup_time:?}"
        );
    }
}

#[derive(Serialize, Deserialize)]
struct Reorganization {
    moves: Vec<MoveRow>,
    originals: Vec<Item>,
    undone: bool,
}

type VerifiedFiles = std::sync::Mutex<std::collections::HashMap<PathBuf, (String, String)>>;

pub struct Archive {
    pub root: PathBuf,
    pub settings: Settings,
    pub id: String,
    connection: Connection,
    _lock: Option<File>,
    import_names: std::collections::HashMap<PathBuf, std::collections::HashSet<String>>,
    verified: std::sync::Arc<VerifiedFiles>,
    xmp_names: std::sync::Mutex<xmp::DirectoryNames>,
    xmp_work: std::sync::Arc<std::sync::atomic::AtomicBool>,
    xmp_exporting: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| format!("Invalid catalog record: {e}"))
}

fn copy_new(source: &Path, destination: &Path, expected: &str) -> Result<()> {
    use std::io::Read;
    let mut input = File::open(source).map_err(|e| e.to_string())?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| {
            format!(
                "Cannot create {} without overwriting: {e}",
                destination.display()
            )
        })?;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0u8; 128 * 1024];
    let copied: Result<String> = (|| {
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|e| format!("Copy failed: {e}"))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| format!("Copy failed: {e}"))?;
            digest.update(&buffer[..count]);
        }
        output
            .sync_all()
            .map_err(|e| format!("Could not flush copied file: {e}"))?;
        Ok(digest.finalize().to_hex().to_string())
    })();
    match copied {
        Ok(actual) if actual == expected => Ok(()),
        Ok(_) => {
            let _ = fs::remove_file(destination);
            Err("Copied bytes failed verification; the source is untouched.".into())
        }
        Err(error) => {
            let _ = fs::remove_file(destination);
            Err(error)
        }
    }
}

impl Archive {
    pub fn open(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            return Err("Choose an existing destination folder.".into());
        }
        let root = paths::canonical(root)?;
        let private = root.join(".singulo");
        if private
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err("Archive administration folder must not be a symlink.".into());
        }
        fs::create_dir_all(&private).map_err(|e| format!("Destination is not writable: {e}"))?;
        for directory in ["staging", "undo", "thumbnails", "previews", "recovery"] {
            let directory = private.join(directory);
            if directory
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err("Archive administration subfolders must not be symlinks.".into());
            }
            fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(private.join("writer.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock_exclusive()
            .map_err(|e| format!("Archive is in use or locking is unsupported: {e}"))?;
        let marker = private.join("archive-id");
        let id = if marker.exists() {
            fs::read_to_string(&marker).map_err(|e| e.to_string())?
        } else {
            let id = Uuid::new_v4().to_string();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker)
                .map_err(|e| e.to_string())?;
            file.write_all(id.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            id
        };
        let connection =
            Connection::open(private.join("catalog.sqlite")).map_err(|e| e.to_string())?;
        connection
            .create_scalar_function(
                "unicode_lower",
                1,
                rusqlite::functions::FunctionFlags::SQLITE_UTF8
                    | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
                |context| Ok(context.get::<String>(0)?.to_lowercase()),
            )
            .map_err(|e| e.to_string())?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY, hash TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS item_hash ON items(hash);
             CREATE INDEX IF NOT EXISTS video_date_duration ON items(
                 state,substr(json_extract(data,'$.media.date'),1,10),
                 json_extract(data,'$.media.durationSeconds'))
                 WHERE json_extract(data,'$.media.kind')='video';
             CREATE TABLE IF NOT EXISTS occurrences (id TEXT PRIMARY KEY, source TEXT NOT NULL, hash TEXT NOT NULL, data TEXT NOT NULL, UNIQUE(source, hash));
             CREATE TABLE IF NOT EXISTS buckets (bucket INTEGER NOT NULL, item TEXT NOT NULL REFERENCES items(id), UNIQUE(bucket,item));
             CREATE INDEX IF NOT EXISTS bucket_lookup ON buckets(bucket);
             CREATE TABLE IF NOT EXISTS reviews (item TEXT NOT NULL, candidate TEXT NOT NULL, reason TEXT NOT NULL, UNIQUE(item,candidate));
             CREATE TABLE IF NOT EXISTS companions (owner TEXT NOT NULL, child TEXT NOT NULL, UNIQUE(owner,child));
             CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS exported_xmp (item TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS xmp_operations (item TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS xmp_dirty (item TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS xmp_baselines (item TEXT PRIMARY KEY, base TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS import_items (job TEXT NOT NULL, item TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(job,item));
             CREATE INDEX IF NOT EXISTS import_item_refs ON import_items(item,job);
             CREATE INDEX IF NOT EXISTS item_keeper ON items(json_extract(data,'$.keeper'));
             CREATE INDEX IF NOT EXISTS occurrence_item ON occurrences(json_extract(data,'$.item'));
             CREATE INDEX IF NOT EXISTS occurrence_status ON occurrences(json_extract(data,'$.status'));
             CREATE INDEX IF NOT EXISTS companion_child ON companions(child);
             CREATE INDEX IF NOT EXISTS library_order ON items(state,json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'));
             CREATE INDEX IF NOT EXISTS library_visible ON items(state,json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'))
                 WHERE json_extract(data,'$.media.kind')!='sidecar';
             CREATE INDEX IF NOT EXISTS library_month ON items(state,coalesce(substr(json_extract(data,'$.media.date'),1,7),'unknown'),json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'),id)
                 WHERE json_extract(data,'$.media.kind')!='sidecar';
             CREATE TABLE IF NOT EXISTS source_groups (source TEXT PRIMARY KEY, item TEXT NOT NULL, group_key TEXT NOT NULL, content_id TEXT);
             CREATE INDEX IF NOT EXISTS source_group_key ON source_groups(group_key);
             CREATE INDEX IF NOT EXISTS source_content_id ON source_groups(content_id);
             CREATE TABLE IF NOT EXISTS totals (state TEXT PRIMARY KEY, count INTEGER NOT NULL, bytes INTEGER NOT NULL, visible INTEGER NOT NULL);
             INSERT OR IGNORE INTO totals SELECT state,COUNT(*),SUM(json_extract(data,'$.media.bytes')),
                 SUM(json_extract(data,'$.media.kind')!='sidecar') FROM items GROUP BY state;
             CREATE TRIGGER IF NOT EXISTS item_totals_insert AFTER INSERT ON items BEGIN
                 INSERT INTO totals VALUES(NEW.state,1,json_extract(NEW.data,'$.media.bytes'),json_extract(NEW.data,'$.media.kind')!='sidecar')
                 ON CONFLICT(state) DO UPDATE SET count=count+1,bytes=bytes+excluded.bytes,visible=visible+excluded.visible;
             END;
             CREATE TRIGGER IF NOT EXISTS item_totals_update AFTER UPDATE ON items BEGIN
                 UPDATE totals SET count=count-1,bytes=bytes-json_extract(OLD.data,'$.media.bytes'),
                     visible=visible-(json_extract(OLD.data,'$.media.kind')!='sidecar') WHERE state=OLD.state;
                 INSERT INTO totals VALUES(NEW.state,1,json_extract(NEW.data,'$.media.bytes'),json_extract(NEW.data,'$.media.kind')!='sidecar')
                 ON CONFLICT(state) DO UPDATE SET count=count+1,bytes=bytes+excluded.bytes,visible=visible+excluded.visible;
             END;"
        ).map_err(|e| e.to_string())?;
        connection
            .execute_batch(
                "CREATE TRIGGER IF NOT EXISTS xmp_item_insert AFTER INSERT ON items
             WHEN EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value='true') BEGIN
                 INSERT INTO xmp_dirty VALUES(NEW.id) ON CONFLICT(item) DO NOTHING;
             END;
             CREATE TRIGGER IF NOT EXISTS xmp_item_update AFTER UPDATE ON items
             WHEN EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value='true') BEGIN
                 INSERT INTO xmp_dirty VALUES(NEW.id) ON CONFLICT(item) DO NOTHING;
                 INSERT INTO xmp_dirty SELECT child FROM companions WHERE owner=NEW.id ON CONFLICT(item) DO NOTHING;
                 INSERT INTO xmp_dirty SELECT owner FROM companions WHERE child=NEW.id ON CONFLICT(item) DO NOTHING;
             END;
             CREATE TRIGGER IF NOT EXISTS xmp_companion_insert AFTER INSERT ON companions
             WHEN EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value='true') BEGIN
                 INSERT INTO xmp_dirty VALUES(NEW.owner) ON CONFLICT(item) DO NOTHING;
                 INSERT INTO xmp_dirty VALUES(NEW.child) ON CONFLICT(item) DO NOTHING;
             END;",
            )
            .map_err(|e| e.to_string())?;
        let settings = connection
            .query_row("SELECT value FROM config WHERE key='settings'", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .map(|v| parse(&v))
            .transpose()?
            .unwrap_or_default();
        paths::validate(&settings)?;
        let (xmp_work, xmp_exporting): (bool, bool) = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value IN ('true','removing'))
                 OR EXISTS(SELECT 1 FROM xmp_operations),
                 EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value='true')",
            [], |r| Ok((r.get(0)?, r.get(1)?))
        ).map_err(|e| e.to_string())?;
        let mut archive = Self {
            root,
            settings,
            id,
            connection,
            _lock: Some(lock),
            import_names: std::collections::HashMap::new(),
            verified: Default::default(),
            xmp_names: Default::default(),
            xmp_work: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(xmp_work)),
            xmp_exporting: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(xmp_exporting)),
        };
        let version: u32 = archive
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version < 3 {
            archive.transaction(|archive| {
                let rows: Vec<(String, String, Option<String>)> = {
                    let mut query = archive
                        .connection
                        .prepare(
                            "SELECT o.source, json_extract(o.data,'$.item'), json_extract(i.data,'$.media.metadata')
                             FROM occurrences o JOIN items i ON i.id=json_extract(o.data,'$.item')",
                        )
                        .map_err(|e| e.to_string())?;
                    query
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .map_err(|e| e.to_string())?
                        .collect::<std::result::Result<_, _>>()
                        .map_err(|e| e.to_string())?
                };
                let mut stmt = archive
                    .connection
                    .prepare(
                        "INSERT INTO source_groups VALUES(?1,?2,?3,?4)
                         ON CONFLICT(source) DO UPDATE SET item=excluded.item,group_key=excluded.group_key,content_id=excluded.content_id",
                    )
                    .map_err(|e| e.to_string())?;
                for (source, item, raw_meta) in rows {
                    let meta: serde_json::Value = raw_meta
                        .as_deref()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(serde_json::Value::Null);
                    let content_id =
                        media::value(&meta, "ContentIdentifier").and_then(|v| v.as_str());
                    stmt.execute(params![
                        source,
                        item,
                        Self::group_key(Path::new(&source)),
                        content_id
                    ])
                    .map_err(|e| e.to_string())?;
                }
                drop(stmt);
                archive
                    .connection
                    .execute_batch("PRAGMA user_version=3;")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
        }
        Ok(archive)
    }

    pub fn check(&self) -> Result<()> {
        let id = fs::read_to_string(self.root.join(".singulo/archive-id"))
            .map_err(|e| format!("Selected archive is disconnected or inaccessible: {e}"))?;
        if id != self.id {
            return Err("The destination is no longer the selected archive.".into());
        }
        self.connection
            .query_row("SELECT 1", [], |_| Ok(()))
            .map_err(|e| format!("Catalog connection is unavailable: {e}"))?;
        Ok(())
    }

    pub fn begin_import(&mut self) {
        self.import_names.clear();
    }

    pub fn release_import_path(&mut self, relative: &Path) {
        if let Ok(target) = paths::inside(&self.root, relative)
            && let Some(parent) = target.parent()
            && let Some(name) = relative.file_name()
            && let Some(names) = self.import_names.get_mut(parent)
        {
            names.remove(&name.to_string_lossy().to_lowercase());
        }
    }

    pub fn record_import_path(&mut self, relative: &Path) {
        if let Ok(target) = paths::inside(&self.root, relative)
            && let Some(parent) = target.parent()
            && let Some(name) = relative.file_name()
        {
            self.import_names
                .entry(parent.to_path_buf())
                .or_default()
                .insert(name.to_string_lossy().to_lowercase());
        }
    }

    pub fn unique_import_path(&mut self, wanted: &Path, id: &str) -> Result<PathBuf> {
        let target = paths::inside(&self.root, wanted)?;
        let parent = target
            .parent()
            .ok_or("Missing output parent.")?
            .to_path_buf();
        if !self.import_names.contains_key(&parent) {
            let mut names = std::collections::HashSet::new();
            if parent.exists() {
                for entry in fs::read_dir(&parent).map_err(|e| e.to_string())? {
                    names.insert(
                        entry
                            .map_err(|e| e.to_string())?
                            .file_name()
                            .to_string_lossy()
                            .to_lowercase(),
                    );
                }
            }
            self.import_names.insert(parent.clone(), names);
        }
        let names = self
            .import_names
            .get_mut(&parent)
            .ok_or("Missing directory name cache.")?;
        let name = wanted
            .file_name()
            .ok_or("Missing output filename.")?
            .to_string_lossy()
            .to_lowercase();
        let result = if names.contains(&name) || target.exists() {
            let stem = wanted
                .file_stem()
                .ok_or("Missing output stem.")?
                .to_string_lossy();
            let ext = wanted
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            wanted.with_file_name(format!("{stem}__{id}{ext}"))
        } else {
            wanted.to_path_buf()
        };
        if result != wanted && paths::inside(&self.root, &result)?.exists() {
            return Err("Collision at generated output filename.".into());
        }
        names.insert(
            result
                .file_name()
                .ok_or("Missing output filename.")?
                .to_string_lossy()
                .to_lowercase(),
        );
        Ok(result)
    }

    pub fn save_settings(&mut self, settings: Settings) -> Result<()> {
        self.check()?;
        paths::validate(&settings)?;
        self.connection
            .execute(
                "INSERT OR REPLACE INTO config VALUES ('settings',?1)",
                [json(&settings)?],
            )
            .map_err(|e| e.to_string())?;
        self.settings = settings;
        self.backup()
    }

    pub fn item(&self, id: &str) -> Result<Item> {
        let data: String = self
            .connection
            .query_row("SELECT data FROM items WHERE id=?1", [id], |r| r.get(0))
            .map_err(|e| format!("Archived item not found: {e}"))?;
        parse(&data)
    }

    pub(crate) fn transaction<T>(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if !self.connection.is_autocommit() {
            return action(self);
        }
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        match action(self) {
            Ok(value) => {
                if let Err(error) = self.connection.execute_batch("COMMIT") {
                    self.connection
                        .execute_batch("ROLLBACK")
                        .map_err(|rollback| {
                            format!("Commit failed: {error}; rollback failed: {rollback}")
                        })?;
                    return Err(format!("Catalog commit failed: {error}"));
                }
                Ok(value)
            }
            Err(error) => {
                self.connection
                    .execute_batch("ROLLBACK")
                    .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
                Err(error)
            }
        }
    }

    pub fn totals(&self, state: &str) -> Result<(usize, u64, usize)> {
        self.check()?;
        let (count, bytes, visible): (i64, i64, i64) = self
            .connection
            .query_row(
                "SELECT count,bytes,visible FROM totals WHERE state=?1",
                [state],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or((0, 0, 0));
        Ok((
            count.try_into().map_err(|_| "Invalid catalog count.")?,
            bytes
                .try_into()
                .map_err(|_| "Invalid catalog byte total.")?,
            visible
                .try_into()
                .map_err(|_| "Invalid catalog visible count.")?,
        ))
    }

    pub fn months(&self, search: &str, filter: &str) -> Result<Vec<LibraryMonth>> {
        self.check()?;
        let (state, condition) = library_filter(filter);
        let mut query = self.connection.prepare_cached(&format!(
            "SELECT coalesce(substr(json_extract(data,'$.media.date'),1,7),'unknown') AS month, count(*)
             FROM items WHERE state=?1 AND json_extract(data,'$.media.kind')!='sidecar' {condition} {LIBRARY_SEARCH}
             GROUP BY month ORDER BY month='unknown', month DESC"
        )).map_err(|e| e.to_string())?;
        query
            .query_map(params![state, search.to_lowercase()], |row| {
                Ok(LibraryMonth {
                    month: row.get(0)?,
                    count: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn page(
        &self,
        search: &str,
        filter: &str,
        month: Option<&str>,
        offset: usize,
    ) -> Result<Vec<Item>> {
        self.check()?;
        let (state, condition) = library_filter(filter);
        let month_condition = if month.is_some() {
            "AND coalesce(substr(json_extract(data,'$.media.date'),1,7),'unknown')=?3"
        } else {
            "AND ?3 IS NULL"
        };
        let mut query = self.connection.prepare_cached(&format!(
            "SELECT data FROM items WHERE state=?1 AND json_extract(data,'$.media.kind')!='sidecar' {condition}
             {LIBRARY_SEARCH}
             {month_condition}
             ORDER BY json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'),id LIMIT 60 OFFSET ?4"
        )).map_err(|e| e.to_string())?;
        query
            .query_map(
                params![
                    state,
                    search.to_lowercase(),
                    month,
                    i64::try_from(offset).map_err(|_| "Library offset is too large.")?
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| e.to_string())?
            .map(|r| parse(&r.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn occurrence_id(&self, source: &Path, hash: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT id FROM occurrences WHERE source=?1 AND hash=?2",
                params![source.to_string_lossy(), hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn item_occurrences(&self, item: &str) -> Result<Vec<Occurrence>> {
        let mut query = self
            .connection
            .prepare_cached("SELECT data FROM occurrences WHERE json_extract(data,'$.item')=?1")
            .map_err(|e| e.to_string())?;
        query
            .query_map([item], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| parse(&r.map_err(|e| e.to_string())?))
            .collect()
    }

    fn group_key(source: &Path) -> String {
        let stripped = source.with_extension("");
        let base = if source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("xmp"))
            && media::kind(&stripped).is_some()
        {
            stripped.with_extension("")
        } else {
            stripped
        };
        base.to_string_lossy().to_lowercase()
    }

    fn index_source(&self, occurrence: &Occurrence) -> Result<()> {
        let item = self.item(&occurrence.item)?;
        let content_id =
            media::value(&item.media.metadata, "ContentIdentifier").and_then(|v| v.as_str());
        self.connection.execute("INSERT INTO source_groups VALUES(?1,?2,?3,?4)
            ON CONFLICT(source) DO UPDATE SET item=excluded.item,group_key=excluded.group_key,content_id=excluded.content_id",
            params![occurrence.source,occurrence.item,Self::group_key(Path::new(&occurrence.source)),content_id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn sidecar_owner(&self, source: &Path) -> Result<Option<Item>> {
        let id: Option<String> = self.connection.query_row(
            "SELECT s.item FROM source_groups s JOIN items i ON i.id=s.item WHERE s.group_key=?1
             AND json_extract(i.data,'$.media.kind')!='sidecar' LIMIT 1",
            [Self::group_key(source)], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        id.map(|id| self.item(&id)).transpose()
    }

    pub fn verified_file(&self, path: &Path, expected_hash: &str) -> Result<bool> {
        if !path.is_file() {
            return Ok(false);
        }
        let id = media::identity(path)?;
        if let Ok(cache) = self.verified.lock()
            && cache
                .get(path)
                .is_some_and(|(cached_id, cached_hash)| cached_id == &id && cached_hash == expected_hash)
        {
            return Ok(true);
        }
        let actual = media::hash(path)?;
        if media::identity(path)? == id
            && let Ok(mut cache) = self.verified.lock()
        {
            if cache.len() > 200_000 {
                cache.clear();
            }
            cache.insert(path.to_path_buf(), (id, actual.clone()));
        }
        Ok(actual == expected_hash)
    }

    fn record_verified(&self, path: &Path, hash: &str) {
        if let Ok(id) = media::identity(path)
            && let Ok(mut cache) = self.verified.lock()
        {
            if cache.len() > 200_000 {
                cache.clear();
            }
            cache.insert(path.to_path_buf(), (id, hash.to_string()));
        }
    }

    pub fn exact(&self, hash: &str) -> Result<Option<Item>> {
        self.exact_inner(hash, false)
    }

    pub fn exact_cached(&self, hash: &str) -> Result<Option<Item>> {
        self.exact_inner(hash, true)
    }

    fn exact_inner(&self, hash: &str, cached: bool) -> Result<Option<Item>> {
        let mut query = self
            .connection
            .prepare_cached(
                "SELECT data FROM items WHERE hash=?1 AND state!='purged' ORDER BY state='active' DESC",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([hash], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let item: Item = parse(&row.map_err(|e| e.to_string())?)?;
            let path = paths::inside(&self.root, Path::new(&item.path))?;
            let matches = if cached {
                self.verified_file(&path, hash)?
            } else {
                path.is_file() && media::hash(&path)? == hash
            };
            if matches {
                if !cached {
                    self.record_verified(&path, hash);
                }
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    pub fn occurrence(&self, id: &str) -> Result<Occurrence> {
        let data: String = self
            .connection
            .query_row("SELECT data FROM occurrences WHERE id=?1", [id], |r| r.get(0))
            .map_err(|e| format!("Source occurrence not found: {e}"))?;
        parse(&data)
    }

    fn occurrences_by_status(&self, status: &str) -> Result<Vec<Occurrence>> {
        let mut query = self
            .connection
            .prepare_cached("SELECT data FROM occurrences WHERE json_extract(data,'$.status')=?1")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([status], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| parse(&row.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn record_occurrence(&mut self, occurrence: &Occurrence) -> Result<()> {
        self.transaction(|archive| archive.record_occurrence_inner(occurrence))
    }

    pub fn record_occurrences(&mut self, occurrences: &[Occurrence]) -> Result<()> {
        self.transaction(|archive| {
            for occurrence in occurrences {
                archive.record_occurrence_inner(occurrence)?;
            }
            Ok(())
        })
    }

    pub fn record_import_occurrences(
        &mut self,
        job: &str,
        occurrences: &[Occurrence],
    ) -> Result<()> {
        self.transaction(|archive| {
            archive.record_occurrences(occurrences)?;
            for occurrence in occurrences {
                archive.record_import_item(job, &occurrence.item, false)?;
            }
            Ok(())
        })
    }

    fn record_import_item(&self, job: &str, item: &str, created: bool) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO import_items(job,item,created) VALUES(?1,?2,?3)
             ON CONFLICT(job,item) DO UPDATE SET created=max(created,excluded.created)",
                params![job, item, created],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn record_occurrence_inner(&mut self, occurrence: &Occurrence) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO occurrences(id,source,hash,data) VALUES(?1,?2,?3,?4)
             ON CONFLICT(source,hash) DO UPDATE SET data=excluded.data",
                params![
                    occurrence.id,
                    occurrence.source,
                    occurrence.hash,
                    json(occurrence)?
                ],
            )
            .map_err(|e| e.to_string())?;
        let mut item = self.item(&occurrence.item)?;
        let mut folder_parts = vec![occurrence.album.clone()];
        if let Some(parent) = Path::new(&occurrence.relative).parent() {
            folder_parts.extend(parent.components().filter_map(|part| match part {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
                _ => None,
            }));
        }
        let (folder_date, folder_labels) = media::folder_path_info(&folder_parts);
        let mut labels = Vec::new();
        if !folder_labels.is_empty() {
            labels.push(folder_labels.join("/"));
            labels.extend(folder_labels);
        }
        let mut changed = false;
        for label in labels {
            if !item.albums.contains(&label) {
                item.albums.push(label);
                changed = true;
            }
        }
        if item.media.date.is_none()
            && let Some(date) = folder_date
        {
            item.media.date = Some(date);
            item.media.date_source = "estimated: folder date".into();
            item.media
                .warnings
                .retain(|warning| warning != "No usable date. Set the date manually if known.");
            changed = true;
        }
        if changed {
            self.connection
                .execute(
                    "UPDATE items SET data=?1 WHERE id=?2",
                    params![json(&item)?, item.id],
                )
                .map_err(|e| e.to_string())?;
        }
        self.index_source(occurrence)?;
        self.link_companions(occurrence)
    }

    fn store(&mut self, item: &Item) -> Result<()> {
        self.connection.execute(
            "INSERT INTO items VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET hash=excluded.hash,state=excluded.state,data=excluded.data",
            params![item.id, item.media.hash, item.state, json(item)?]
        ).map_err(|e| e.to_string())?;
        self.connection
            .execute("DELETE FROM buckets WHERE item=?1", [&item.id])
            .map_err(|e| e.to_string())?;
        if item.state == "active"
            && let Some(signature) = &item.media.signature
        {
            let hash = u64::from_str_radix(signature, 16).map_err(|e| e.to_string())?;
            for (index, byte) in hash.to_le_bytes().iter().enumerate() {
                self.connection
                    .execute(
                        "INSERT OR IGNORE INTO buckets VALUES (?1,?2)",
                        params![index as i64 * 256 + *byte as i64, item.id],
                    )
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn link_companions(&mut self, occurrence: &Occurrence) -> Result<()> {
        let item = self.item(&occurrence.item)?;
        let content_id =
            media::value(&item.media.metadata, "ContentIdentifier").and_then(|v| v.as_str());
        self.connection
            .execute(
                "INSERT OR IGNORE INTO companions
             SELECT item,?1 FROM source_groups WHERE group_key=?2 AND item!=?1",
                params![
                    occurrence.item,
                    Self::group_key(Path::new(&occurrence.source))
                ],
            )
            .map_err(|e| e.to_string())?;
        if let Some(content_id) = content_id {
            self.connection.execute(
                "INSERT OR IGNORE INTO companions SELECT item,?1 FROM source_groups WHERE content_id=?2 AND item!=?1",
                params![occurrence.item,content_id]).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn companions(&self, id: &str) -> Result<Vec<String>> {
        let mut query = self.connection.prepare_cached(
            "SELECT child FROM companions WHERE owner=?1 UNION SELECT owner FROM companions WHERE child=?1"
        ).map_err(|e| e.to_string())?;
        query
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| r.map_err(|e| e.to_string()))
            .collect()
    }

    pub fn video_candidates(&self, incoming: &Media) -> Result<Vec<Item>> {
        let Some(duration) = incoming
            .duration_seconds
            .filter(|d| d.is_finite() && *d > 0.0)
        else {
            return Ok(Vec::new());
        };
        let Some(date) = incoming.date.as_deref().and_then(|date| date.get(..10)) else {
            return Ok(Vec::new());
        };
        if incoming.kind != "video" || !incoming.date_source.starts_with("capture") {
            return Ok(Vec::new());
        }
        let tolerance = (duration * 0.01).max(1.0);
        let mut query = self
            .connection
            .prepare_cached(
                "SELECT data FROM items WHERE state='active'
             AND json_extract(data,'$.media.kind')='video'
             AND substr(json_extract(data,'$.media.date'),1,10)=?1
             AND json_extract(data,'$.media.durationSeconds') BETWEEN ?2 AND ?3
             AND json_extract(data,'$.media.dateSource') LIKE 'capture%'
             ORDER BY abs(json_extract(data,'$.media.durationSeconds')-?4),id LIMIT 10",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map(
                params![date, duration - tolerance, duration + tolerance, duration],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| e.to_string())?;
        rows.map(|row| parse(&row.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn candidates(&self, incoming: &Media) -> Result<Vec<Item>> {
        let Some(signature) = &incoming.signature else {
            return Ok(Vec::new());
        };
        let hash = u64::from_str_radix(signature, 16).map_err(|e| e.to_string())?;
        let buckets = hash
            .to_le_bytes()
            .iter()
            .enumerate()
            .map(|(i, b)| (i * 256 + *b as usize).to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT items.data FROM items JOIN
                 (SELECT item,COUNT(*) AS matches FROM buckets WHERE bucket IN ({buckets})
                  GROUP BY item HAVING COUNT(*)>=2 ORDER BY matches DESC LIMIT 200) hits ON items.id=hits.item
                 WHERE items.state='active'"
            ))
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut candidates = Vec::new();
        for row in rows {
            let item: Item = parse(&row.map_err(|e| e.to_string())?)?;
            if item.media.kind == incoming.kind
                && media::aspect_compatible(
                    incoming.width,
                    incoming.height,
                    item.media.width,
                    item.media.height,
                )
                && item
                    .media
                    .signature
                    .as_ref()
                    .and_then(|s| media::distance(signature, s))
                    .is_some_and(|d| d <= 6)
            {
                candidates.push(item);
            }
        }
        candidates.sort_by_key(|item| {
            media::distance(signature, item.media.signature.as_deref().unwrap_or("")).unwrap_or(64)
        });
        candidates.truncate(10);
        Ok(candidates)
    }

    pub fn add_review(&mut self, item: &str, candidate: &str, reason: &str) -> Result<()> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO reviews VALUES(?1,?2,?3)",
                params![item, candidate, reason],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn reviews(&self) -> Result<Vec<serde_json::Value>> {
        let mut query = self
            .connection
            .prepare(
                "SELECT r.item,r.candidate,r.reason,
                 json_extract(i.data,'$.originalName'),json_extract(i.data,'$.media.kind'),
                 json_extract(c.data,'$.originalName'),json_extract(c.data,'$.media.kind')
                 FROM reviews r JOIN items i ON i.id=r.item
             JOIN items c ON c.id=r.candidate WHERE i.state='active' AND c.state='active'",
            )
            .map_err(|e| e.to_string())?;
        query.query_map([], |r| Ok(serde_json::json!({
            "item": r.get::<_,String>(0)?, "candidate": r.get::<_,String>(1)?, "reason": r.get::<_,String>(2)?,
            "itemPreview": {"id": r.get::<_,String>(0)?, "originalName": r.get::<_,String>(3)?, "media": {"kind": r.get::<_,String>(4)?}},
            "candidatePreview": {"id": r.get::<_,String>(1)?, "originalName": r.get::<_,String>(5)?, "media": {"kind": r.get::<_,String>(6)?}}
        }))).map_err(|e| e.to_string())?.map(|r| r.map_err(|e| e.to_string())).collect()
    }

    pub fn dismiss_review(&mut self, item: &str, candidate: &str) -> Result<()> {
        self.connection.execute("DELETE FROM reviews WHERE (item=?1 AND candidate=?2) OR (item=?2 AND candidate=?1)",
            params![item, candidate]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn transfer_reviews(&mut self, from: &str, to: &str) -> Result<()> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO reviews
                 SELECT ?2, candidate, reason FROM reviews WHERE item=?1 AND candidate!=?2
                 UNION ALL
                 SELECT item, ?2, reason FROM reviews WHERE candidate=?1 AND item!=?2",
                params![from, to],
            )
            .map_err(|e| e.to_string())?;
        self.connection
            .execute("DELETE FROM reviews WHERE item=?1 OR candidate=?1", [from])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn place(
        &mut self,
        source: &Path,
        item: Item,
        occurrence: Option<Occurrence>,
        old_path: Option<String>,
    ) -> Result<()> {
        self.place_with_job(source, item, occurrence, old_path, None)
    }

    pub fn place_import(
        &mut self,
        source: &Path,
        item: Item,
        occurrence: Occurrence,
        job: &str,
    ) -> Result<()> {
        self.place_with_job(source, item, Some(occurrence), None, Some(job.into()))
    }

    fn place_with_job(
        &mut self,
        source: &Path,
        item: Item,
        occurrence: Option<Occurrence>,
        old_path: Option<String>,
        import_job: Option<String>,
    ) -> Result<()> {
        self.check()?;
        let stage_relative = format!(".singulo/staging/{}", Uuid::new_v4());
        let stage = paths::inside(&self.root, Path::new(&stage_relative))?;
        copy_new(source, &stage, &item.media.hash)?;
        let operation = Operation {
            item,
            occurrence,
            stage: stage_relative,
            old_path,
            import_job,
        };
        let id = Uuid::new_v4().to_string();
        self.connection
            .execute(
                "INSERT INTO operations VALUES(?1,?2)",
                params![id, json(&operation)?],
            )
            .map_err(|e| e.to_string())?;
        self.finish_operation(&id, &operation)
    }

    fn finish_operation(&mut self, id: &str, operation: &Operation) -> Result<()> {
        self.check()?;
        let target = paths::inside(&self.root, Path::new(&operation.item.path))?;
        let stage = paths::inside(&self.root, Path::new(&operation.stage))?;
        fs::create_dir_all(target.parent().ok_or("Missing target folder.")?)
            .map_err(|e| e.to_string())?;
        if !target.exists() {
            match fs::hard_link(&stage, &target) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
                    ) || error.raw_os_error().is_some_and(|code| {
                        if cfg!(windows) {
                            matches!(code, 1 | 50)
                        } else {
                            matches!(code, 18 | 38 | 45 | 95)
                        }
                    }) =>
                {
                    copy_new(&stage, &target, &operation.item.media.hash)?;
                }
                Err(error) => {
                    return Err(format!(
                        "Could not place verified copy without overwriting: {error}"
                    ));
                }
            }
            #[cfg(unix)]
            File::open(target.parent().ok_or("Missing target folder.")?)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("Could not flush final archive directory: {e}"))?;
        } else if media::hash(&target)? != operation.item.media.hash {
            return Err(format!(
                "Interrupted operation has a conflicting or incomplete target at {}. Both staging and source are retained; resolve this file before recovery.",
                target.display()
            ));
        }
        self.transaction(|archive| {
            archive.store(&operation.item)?;
            if let Some(occurrence) = &operation.occurrence {
                archive.record_occurrence(occurrence)?;
            }
            if let Some(job) = &operation.import_job {
                archive.record_import_item(job, &operation.item.id, true)?;
            }
            Ok(())
        })?;
        self.record_verified(&target, &operation.item.media.hash);
        if let Some(old) = &operation.old_path {
            let old = paths::inside(&self.root, Path::new(old))?;
            if old != target && old.exists() {
                if media::hash(&old)? != operation.item.media.hash {
                    return Err(format!(
                        "Original archive path changed during relocation: {}",
                        old.display()
                    ));
                }
                fs::remove_file(&old).map_err(|e| e.to_string())?;
                if let Ok(mut cache) = self.verified.lock() {
                    cache.remove(&old);
                }
            }
        }
        if stage.exists() {
            fs::remove_file(stage).map_err(|e| e.to_string())?;
        }
        self.connection
            .execute("DELETE FROM operations WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn recover(&mut self) -> Result<Vec<String>> {
        let operations: Vec<(String, String)> = {
            let mut query = self
                .connection
                .prepare("SELECT id,data FROM operations")
                .map_err(|e| e.to_string())?;
            query
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<std::result::Result<_, _>>()
                .map_err(|e| e.to_string())?
        };
        let mut errors = Vec::new();
        for (id, data) in operations {
            let operation = parse(&data)?;
            if let Err(error) = self.finish_operation(&id, &operation) {
                errors.push(error);
            }
        }
        let staged_occurrences: Vec<Occurrence> = {
            let mut query = self
                .connection
                .prepare("SELECT data FROM occurrences WHERE json_extract(data,'$.status') LIKE 'staged:%'")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.map(|r| parse(&r.map_err(|e| e.to_string())?))
                .collect::<Result<Vec<_>>>()?
        };
        for mut occurrence in staged_occurrences {
            if let Some(stage) = occurrence.status.strip_prefix("staged:") {
                let stage = PathBuf::from(stage);
                if stage.exists() {
                    match self.restore_source(
                        &stage,
                        Path::new(&occurrence.source),
                        &occurrence.hash,
                    ) {
                        Ok(()) => {
                            occurrence.status = "pending".into();
                            occurrence.identity = media::identity(Path::new(&occurrence.source))?;
                            self.record_occurrence(&occurrence)?;
                        }
                        Err(error) => errors.push(format!(
                            "Source cleanup needs recovery at {}: {error}",
                            stage.display()
                        )),
                    }
                } else {
                    errors.push(format!("Interrupted source cleanup at {}; check OS trash. Its result is not confirmed.", occurrence.source));
                }
            }
        }
        let pending_purges: Vec<Item> = {
            let mut query = self
                .connection
                .prepare("SELECT data FROM items WHERE state='purge-pending'")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.map(|r| parse(&r.map_err(|e| e.to_string())?))
                .collect::<Result<Vec<_>>>()?
        };
        for mut item in pending_purges {
            let path = paths::inside(&self.root, Path::new(&item.path))?;
            item.state = if path.exists() { "undo" } else { "purged" }.into();
            self.store(&item)?;
            errors.push(format!(
                "Reconciled interrupted archive purge for {}. Retained files require a new confirmation.",
                item.original_name
            ));
        }
        let pending_trash: Vec<Item> = {
            let mut query = self
                .connection
                .prepare("SELECT data FROM items WHERE state='trash-pending'")
                .map_err(|e| e.to_string())?;
            query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .map(|row| parse(&row.map_err(|e| e.to_string())?))
                .collect::<Result<_>>()?
        };
        for mut item in pending_trash {
            let path = paths::inside(&self.root, Path::new(&item.path))?;
            item.state = if path.exists() { "undo" } else { "purged" }.into();
            self.store(&item)?;
            errors.push(format!(
                "Interrupted import clearing for {}. Check OS Trash; any retained archive copy is in spare copies. Nothing will be trashed automatically.",
                item.original_name
            ));
        }
        errors.extend(self.xmp_recovery_attention()?);
        Ok(errors)
    }

    pub fn relocate(
        &mut self,
        id: &str,
        wanted: PathBuf,
        state: &str,
        keeper: Option<String>,
    ) -> Result<()> {
        let mut item = self.item(id)?;
        if item.state == "purged" {
            return Err("This original has been purged.".into());
        }
        let source = paths::inside(&self.root, Path::new(&item.path))?;
        let old = item.path.clone();
        let path = paths::unique_for(
            &self.root,
            &wanted,
            &Uuid::new_v4().to_string(),
            Some(Path::new(&old)),
        )?;
        item.path = path.to_string_lossy().replace('\\', "/");
        item.state = state.into();
        item.keeper = keeper;
        self.place(&source, item, None, Some(old))
    }

    pub fn move_to_undo(&mut self, id: &str, keeper: &str) -> Result<()> {
        if id == keeper {
            return Err("A photo cannot replace itself.".into());
        }
        let keep = self.item(keeper)?;
        let keep_path = paths::inside(&self.root, Path::new(&keep.path))?;
        if keep.state != "active" || !self.verified_file(&keep_path, &keep.media.hash)? {
            return Err("The chosen keeper is not a verified active copy.".into());
        }
        if !self.companions(id)?.is_empty() {
            return Err("This item has RAW/JPEG, sidecar or Live Photo companions. Keep the set together; automatic removal is disabled.".into());
        }
        let item = self.item(id)?;
        let wanted = PathBuf::from(".singulo/undo")
            .join(id)
            .join(paths::safe_name(&item.original_name));
        self.relocate(id, wanted, "undo", Some(keeper.into()))
    }

    pub fn restore(&mut self, id: &str) -> Result<()> {
        let item = self.item(id)?;
        if item.state != "undo" {
            return Err("Only retained undo copies can be restored.".into());
        }
        let wanted = paths::relative(&item.settings, &item.media, &item.original_name)?;
        self.relocate(id, wanted, "active", None)?;
        self.backup()
    }

    pub fn cleanup_reader(&self) -> Result<Self> {
        self.check()?;
        let catalog = paths::inside(&self.root, Path::new(".singulo/catalog.sqlite"))?;
        let connection = Connection::open_with_flags(catalog, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("Cannot open the catalog for source verification: {e}"))?;
        Ok(Self {
            root: self.root.clone(),
            settings: self.settings.clone(),
            id: self.id.clone(),
            connection,
            _lock: None,
            import_names: std::collections::HashMap::new(),
            verified: Default::default(),
            xmp_names: Default::default(),
            xmp_work: self.xmp_work.clone(),
            xmp_exporting: self.xmp_exporting.clone(),
        })
    }

    pub fn cleanup_rows(&self) -> Result<Vec<CleanupRow>> {
        self.check()?;
        let mut archive_checked = std::collections::HashMap::<String, bool>::new();
        self.occurrences_by_status("pending")?
            .into_iter()
            .map(|o| {
                let check = self.cleanup_check_cached(&o, Some(&mut archive_checked));
                Ok(CleanupRow {
                    id: o.id,
                    source: o.source,
                    root: o.root,
                    bytes: o.bytes,
                    eligible: check.is_ok(),
                    reason: check
                        .err()
                        .unwrap_or_else(|| "Byte-identical archived original verified.".into()),
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub fn cleanup_check(&self, occurrence: &Occurrence) -> Result<()> {
        self.cleanup_check_cached(occurrence, None)
    }

    fn cleanup_source_safe(&self, occurrence: &Occurrence) -> Result<()> {
        self.check()?;
        let source = Path::new(&occurrence.source);
        let root = paths::canonical(Path::new(&occurrence.root))
            .map_err(|e| format!("Source root unavailable: {e}"))?;
        let canonical =
            paths::canonical(source).map_err(|e| format!("Source unavailable: {e}"))?;
        if !canonical.starts_with(&root) || canonical.starts_with(&self.root) {
            return Err("Source identity or selected root is unsafe.".into());
        }
        let mut parent = source.parent();
        while let Some(path) = parent {
            if path
                .symlink_metadata()
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("Source path contains a symlink.".into());
            }
            if path == root {
                break;
            }
            parent = path.parent();
        }
        if media::identity(source)? != occurrence.identity {
            return Err("Source changed since import.".into());
        }
        Ok(())
    }

    fn cleanup_check_cached(
        &self,
        occurrence: &Occurrence,
        archive_checked: Option<&mut std::collections::HashMap<String, bool>>,
    ) -> Result<()> {
        self.cleanup_source_safe(occurrence)?;
        let source = Path::new(&occurrence.source);
        if media::hash(source)? != occurrence.hash {
            return Err("Source content no longer matches the imported original.".into());
        }
        let has_exact = match archive_checked {
            Some(cache) => match cache.get(&occurrence.hash) {
                Some(&ok) => ok,
                None => {
                    let ok = self.exact(&occurrence.hash)?.is_some();
                    cache.insert(occurrence.hash.clone(), ok);
                    ok
                }
            },
            None => self.exact(&occurrence.hash)?.is_some(),
        };
        if !has_exact {
            return Err("No readable byte-identical archived original remains.".into());
        }
        if media::identity(source)? != occurrence.identity {
            return Err("Source changed during verification.".into());
        }
        Ok(())
    }

    fn restore_source(&self, stage: &Path, source: &Path, expected: &str) -> Result<()> {
        if source.exists() {
            return Err(format!(
                "A file now occupies {}; staged source is retained.",
                source.display()
            ));
        }
        copy_new(stage, source, expected)?;
        fs::remove_file(stage).map_err(|e| e.to_string())?;
        if let Some(parent) = stage.parent() {
            fs::remove_dir(parent).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn cleanup_one(&mut self, id: &str) -> Result<PathBuf> {
        let mut occurrence = self.occurrence(id)?;
        if occurrence.status != "pending" {
            return Err("Source is not eligible for a new cleanup operation.".into());
        }
        self.cleanup_source_safe(&occurrence)?;
        let source = PathBuf::from(&occurrence.source);
        let parent = source.parent().ok_or("Missing source parent.")?;
        let private = parent.join(format!(".singulo-cleanup-{}", Uuid::new_v4()));
        fs::create_dir(&private).map_err(|e| format!("Cannot safely stage source cleanup: {e}"))?;
        let stage = private.join(source.file_name().ok_or("Missing filename.")?);
        occurrence.status = format!("staged:{}", stage.display());
        self.record_occurrence(&occurrence)?;
        if let Err(error) = fs::rename(&source, &stage) {
            occurrence.status = "pending".into();
            let _ = self.record_occurrence(&occurrence);
            let _ = fs::remove_dir(&private);
            return Err(format!("Source staging failed; it was not trashed: {error}"));
        }
        let validation: Result<()> = (|| {
            if media::identity(&stage)? != occurrence.identity
                || media::hash(&stage)? != occurrence.hash
            {
                return Err("Source changed during cleanup staging.".into());
            }
            if self.exact(&occurrence.hash)?.is_none() {
                return Err("Archive original is no longer verified.".into());
            }
            // Trash the isolated, verified file, never a potentially replaced original pathname.
            crate::recycle::recycle(&stage)?;
            Ok(())
        })();
        if let Err(error) = validation {
            let actual_hash = media::hash(&stage)?;
            match self.restore_source(&stage, &source, &actual_hash) {
                Ok(()) => {
                    occurrence.status = "pending".into();
                    // Preserve the old import identity if a different file raced into the pathname.
                    if actual_hash == occurrence.hash {
                        occurrence.identity = media::identity(&source)?;
                    }
                    self.record_occurrence(&occurrence)?;
                    return Err(format!(
                        "{error} Source restored; nothing was permanently deleted."
                    ));
                }
                Err(restore) => {
                    return Err(format!(
                        "{error} {restore} Recover the source from {}.",
                        stage.display()
                    ));
                }
            }
        }
        occurrence.status = "trashed".into();
        self.record_occurrence(&occurrence)?;
        fs::remove_dir(&private).map_err(|e| {
            format!("File is in trash; empty cleanup folder could not be removed: {e}")
        })?;
        Ok(PathBuf::from(&occurrence.root))
    }

    pub fn remove_empty_subfolders(&self, roots: &[PathBuf]) -> Result<Vec<String>> {
        let mut errors = Vec::new();
        let occurrences = self.occurrences_by_status("trashed")?;
        let mut directories = Vec::new();
        for occurrence in &occurrences {
            let root = PathBuf::from(&occurrence.root);
            if !roots.contains(&root) {
                continue;
            }
            let mut parent = Path::new(&occurrence.source).parent();
            while let Some(path) = parent {
                if path == root || !path.starts_with(&root) {
                    break;
                }
                directories.push(path.to_path_buf());
                parent = path.parent();
            }
        }
        directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        directories.dedup();
        for path in directories {
            if !path.exists() {
                continue;
            }
            if path
                .symlink_metadata()
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                continue;
            }
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                    ) => {}
                Err(e) => errors.push(format!(
                    "Could not remove empty folder {}: {e}",
                    path.display()
                )),
            }
        }
        Ok(errors)
    }

    pub fn purge(&mut self, id: &str, acknowledge_original_loss: bool) -> Result<()> {
        self.check()?;
        let mut item = self.item(id)?;
        if item.state != "undo" {
            return Err("Only retained undo copies can be purged.".into());
        }
        if !acknowledge_original_loss {
            return Err("Purging can destroy the last byte-identical original, including cleaned-up sources. Explicit acknowledgement is required.".into());
        }
        let path = paths::inside(&self.root, Path::new(&item.path))?;
        if media::hash(&path)? != item.media.hash {
            return Err("Undo original has changed; refusing to purge it.".into());
        }
        item.state = "purge-pending".into();
        self.store(&item)?;
        if let Err(error) = fs::remove_file(&path) {
            item.state = "undo".into();
            self.store(&item)?;
            return Err(format!("Undo original was not purged: {error}"));
        }
        if let Ok(mut cache) = self.verified.lock() {
            cache.remove(&path);
        }
        if let Some(parent) = path.parent()
            && parent != self.root
        {
            let _ = fs::remove_dir(parent);
        }
        for cached in [
            self.root
                .join(".singulo/thumbnails")
                .join(format!("{id}-small.jpg")),
            self.root
                .join(".singulo/thumbnails")
                .join(format!("{id}-large.jpg")),
            self.root
                .join(".singulo/previews")
                .join(format!("{id}.mp4")),
        ] {
            let _ = fs::remove_file(cached);
        }
        item.state = "purged".into();
        self.store(&item)?;
        self.backup()
    }

    pub fn move_preview(&self, ids: &[String]) -> Result<Vec<MoveRow>> {
        self.check()?;
        let mut items = Vec::<Item>::new();
        if ids.is_empty() {
            let mut query = self
                .connection
                .prepare("SELECT data FROM items WHERE state='active' ORDER BY rowid ASC")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            for row in rows {
                items.push(parse(&row.map_err(|e| e.to_string())?)?);
            }
        } else {
            let mut seen = std::collections::HashSet::new();
            let mut queue = ids.to_vec();
            let mut index = 0;
            while index < queue.len() {
                let id = queue[index].clone();
                index += 1;
                if !seen.insert(id.clone()) {
                    continue;
                }
                let item = self.item(&id)?;
                if item.state != "active" {
                    continue;
                }
                for companion in self.companions(&id)? {
                    if !seen.contains(&companion)
                        && let Ok(other) = self.item(&companion)
                        && other.state == "active"
                        && other.media.kind == "sidecar"
                    {
                        queue.push(companion);
                    }
                }
                items.push(item);
            }
        }
        items.sort_by_key(|item| item.media.kind == "sidecar");

        let mut managed_identities = std::collections::HashSet::new();
        for item in &items {
            if let Ok(abs) = paths::inside(&self.root, Path::new(&item.path))
                && let Ok(fid) = media::identity(&abs)
            {
                managed_identities.insert(fid);
            }
        }

        let mut dir_unmanaged =
            std::collections::HashMap::<PathBuf, std::collections::HashSet<String>>::new();
        let unmanaged_conflict =
            |dir_cache: &mut std::collections::HashMap<PathBuf, std::collections::HashSet<String>>,
             wanted: &Path|
             -> Result<bool> {
                let target = paths::inside(&self.root, wanted)?;
                let parent = target
                    .parent()
                    .ok_or("Missing output parent.")?
                    .to_path_buf();
                let name = target
                    .file_name()
                    .ok_or("Missing filename.")?
                    .to_string_lossy()
                    .to_lowercase();
                if !dir_cache.contains_key(&parent) {
                    let mut names = std::collections::HashSet::new();
                    if parent.exists() {
                        for entry in fs::read_dir(&parent).map_err(|e| e.to_string())? {
                            let entry = entry.map_err(|e| e.to_string())?;
                            let occupied = media::identity(&entry.path())
                                .map(|fid| !managed_identities.contains(&fid))
                                .unwrap_or(true);
                            if occupied {
                                names.insert(entry.file_name().to_string_lossy().to_lowercase());
                            }
                        }
                    }
                    dir_cache.insert(parent.clone(), names);
                }
                Ok(dir_cache
                    .get(&parent)
                    .is_some_and(|names| names.contains(&name)))
            };

        let mut planned = std::collections::HashSet::new();
        let mut final_paths = std::collections::HashMap::<String, String>::new();
        let mut result = Vec::new();

        for item in items {
            let wanted = if item.media.kind == "sidecar" {
                let owner_target = self.companions(&item.id)?.into_iter().find_map(|cid| {
                    if let Some(p) = final_paths.get(&cid) {
                        return Some(p.clone());
                    }
                    self.item(&cid)
                        .ok()
                        .filter(|o| o.state == "active" && o.media.kind != "sidecar")
                        .map(|o| o.path)
                });
                if let Some(owner_path) = owner_target {
                    PathBuf::from(owner_path).with_extension("xmp")
                } else {
                    paths::relative(&self.settings, &item.media, &item.original_name)?
                }
            } else {
                paths::relative(&self.settings, &item.media, &item.original_name)?
            };

            let wanted_str = wanted.to_string_lossy().replace('\\', "/");
            let wanted_key = wanted_str.to_lowercase();
            let to_path = if !planned.contains(&wanted_key)
                && !unmanaged_conflict(&mut dir_unmanaged, &wanted)?
            {
                wanted
            } else {
                let suffixed = paths::suffixed(&wanted, &item.id)?;
                let suffixed_key = suffixed.to_string_lossy().replace('\\', "/").to_lowercase();
                if planned.contains(&suffixed_key)
                    || unmanaged_conflict(&mut dir_unmanaged, &suffixed)?
                {
                    return Err("Reorganization has an unresolved collision.".into());
                }
                suffixed
            };
            let to = to_path.to_string_lossy().replace('\\', "/");
            planned.insert(to.to_lowercase());
            final_paths.insert(item.id.clone(), to.clone());
            if to != item.path {
                result.push(MoveRow {
                    id: item.id,
                    from: item.path,
                    to,
                });
            }
        }
        Ok(result)
    }

    fn execute_moves(
        &mut self,
        moves: &[MoveRow],
        settings_for: impl Fn(&str) -> Result<Settings>,
    ) -> Result<()> {
        let mut moving_identities = std::collections::HashSet::new();
        for row in moves {
            let item = self.item(&row.id)?;
            if item.path != row.from {
                return Err(
                    "Library changed since the reorganization preview; preview again.".into(),
                );
            }
            let source = paths::inside(&self.root, Path::new(&row.from))?;
            if media::hash(&source)? != item.media.hash {
                return Err("Archived original changed since import.".into());
            }
            moving_identities.insert(media::identity(&source)?);
        }
        let mut parked = std::collections::HashMap::<String, String>::new();
        for row in moves {
            let target = paths::inside(&self.root, Path::new(&row.to))?;
            if target.exists() {
                let target_id = media::identity(&target).unwrap_or_default();
                if !moving_identities.contains(&target_id) {
                    return Err("A target appeared after preview; preview again.".into());
                }
            }
        }
        // Park any source whose target is currently occupied by a moving item (chains, swaps, or case-only renames).
        let occupied_targets: std::collections::HashSet<String> = moves
            .iter()
            .filter_map(|row| {
                paths::inside(&self.root, Path::new(&row.to))
                    .ok()
                    .filter(|t| t.exists())
                    .map(|_| row.id.clone())
            })
            .collect();
        if !occupied_targets.is_empty() {
            for row in moves {
                let target = paths::inside(&self.root, Path::new(&row.to))?;
                let from_clash = moves
                    .iter()
                    .any(|other| other.to.eq_ignore_ascii_case(&row.from));
                if target.exists() || from_clash {
                    let mut item = self.item(&row.id)?;
                    let source = paths::inside(&self.root, Path::new(&row.from))?;
                    let temp = format!(".singulo/staging/reloc-{}-{}", row.id, Uuid::new_v4());
                    item.path = temp.clone();
                    self.place(&source, item, None, Some(row.from.clone()))?;
                    parked.insert(row.id.clone(), temp);
                }
            }
        }
        for row in moves {
            let mut item = self.item(&row.id)?;
            let current_from = parked
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| row.from.clone());
            let source = paths::inside(&self.root, Path::new(&current_from))?;
            item.path = row.to.clone();
            item.settings = settings_for(&row.id)?;
            self.place(&source, item, None, Some(current_from))?;
        }
        Ok(())
    }

    pub fn reorganize(&mut self, preview: &[MoveRow]) -> Result<()> {
        self.check()?;
        let manifest = self.root.join(".singulo/recovery").join(format!(
            "reorganization-{}-{}.json",
            chrono::Utc::now().timestamp_micros(),
            Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(manifest)
            .map_err(|e| e.to_string())?;
        let originals = preview
            .iter()
            .map(|row| self.item(&row.id))
            .collect::<Result<Vec<_>>>()?;
        file.write_all(
            json(&Reorganization {
                moves: preview.to_vec(),
                originals,
                undone: false,
            })?
            .as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let target_settings = self.settings.clone();
        self.execute_moves(preview, |_| Ok(target_settings.clone()))?;
        self.backup()
    }

    fn latest_reorganization(&self) -> Result<(PathBuf, Reorganization)> {
        let mut files = fs::read_dir(self.root.join(".singulo/recovery"))
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|e| e.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("reorganization-"))
                    && path.extension().is_some_and(|ext| ext == "json")
            })
            .collect::<Vec<_>>();
        files.sort();
        for path in files.into_iter().rev() {
            let record: Reorganization =
                parse(&fs::read_to_string(&path).map_err(|e| e.to_string())?)?;
            if !record.undone {
                return Ok((path, record));
            }
        }
        Err("No reorganization is available to undo.".into())
    }

    pub fn rollback_preview(&self) -> Result<Vec<MoveRow>> {
        let (_, record) = self.latest_reorganization()?;
        let mut moving_identities = std::collections::HashSet::new();
        for row in &record.moves {
            let current = self.item(&row.id)?;
            if current.path == row.from {
                continue;
            }
            if current.path != row.to || current.state != "active" {
                return Err("A reorganized item has changed since the operation. Restore conflicts manually rather than overwriting.".into());
            }
            let current_abs = paths::inside(&self.root, Path::new(&current.path))?;
            if let Ok(fid) = media::identity(&current_abs) {
                moving_identities.insert(fid);
            }
        }
        let mut result = Vec::new();
        for row in record.moves.iter().rev() {
            let current = self.item(&row.id)?;
            if current.path == row.from {
                continue;
            }
            let dest = paths::inside(&self.root, Path::new(&row.from))?;
            if dest.exists() {
                let fid = media::identity(&dest).unwrap_or_default();
                if !moving_identities.contains(&fid) {
                    return Err(format!(
                        "Rollback conflict at {}. Nothing will be overwritten.",
                        row.from
                    ));
                }
            }
            result.push(MoveRow {
                id: row.id.clone(),
                from: row.to.clone(),
                to: row.from.clone(),
            });
        }
        Ok(result)
    }

    pub fn rollback(&mut self, confirmed_preview: &[MoveRow]) -> Result<()> {
        let expected = self.rollback_preview()?;
        if json(&expected)? != json(confirmed_preview)? {
            return Err("Rollback has changed since the preview. Preview again.".into());
        }
        let (path, mut record) = self.latest_reorganization()?;
        let originals = record.originals.clone();
        self.execute_moves(&expected, |id| {
            originals
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.settings.clone())
                .ok_or_else(|| "Missing original layout in recovery manifest.".into())
        })?;
        record.undone = true;
        let temporary = path.with_extension("pending");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(json(&record)?.as_bytes())
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        fs::rename(temporary, path).map_err(|e| e.to_string())?;
        self.backup()
    }

    pub fn correct_date(&mut self, id: &str, date: &str) -> Result<()> {
        let mut visited = std::collections::HashSet::new();
        self.correct_date_inner(id, date, None, &mut visited)?;
        self.backup()
    }

    fn correct_date_inner(
        &mut self,
        id: &str,
        date: &str,
        owner_path: Option<&str>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        if !visited.insert(id.to_string()) {
            return Ok(());
        }
        let mut item = self.item(id)?;
        if item.state != "active" {
            if owner_path.is_some() {
                return Ok(());
            }
            return Err("Restore this original before correcting its date.".into());
        }
        let selected =
            media::valid_date(date).ok_or("Enter a valid non-default date, not in the future.")?;
        let full_date = if selected.len() == 10
            && let Some(suffix) = item
                .media
                .date
                .as_deref()
                .and_then(|d| d.get(10..))
                .filter(|s| s.starts_with('T'))
        {
            format!("{selected}{suffix}")
        } else {
            selected
        };
        let old = item.path.clone();
        let source = paths::inside(&self.root, Path::new(&old))?;
        if !source.is_file() {
            return Err("Archived file is missing on disk.".into());
        }
        item.media.date = Some(full_date.clone());
        item.media.date_source = "manual".into();
        let target = if let Some(owner_path) = owner_path {
            PathBuf::from(owner_path).with_extension("xmp")
        } else {
            paths::relative(&item.settings, &item.media, &item.original_name)?
        };
        let next = paths::unique_for(&self.root, &target, id, Some(Path::new(&old)))?
            .to_string_lossy()
            .replace('\\', "/");
        if next == old {
            self.transaction(|archive| archive.store(&item))?;
        } else {
            item.path = next.clone();
            self.place(&source, item, None, Some(old))?;
        }
        for companion in self.companions(id)? {
            if self.item(&companion)?.media.kind == "sidecar" {
                self.correct_date_inner(&companion, &full_date, Some(&next), visited)?;
            }
        }
        Ok(())
    }

    pub fn save_job(&self, id: &str, data: &serde_json::Value) -> Result<()> {
        self.connection
            .execute(
                "INSERT INTO jobs VALUES(?1,?2)
                 ON CONFLICT(id) DO UPDATE SET data=json_patch(jobs.data,excluded.data)",
                params![id, json(data)?],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn jobs(&self) -> Result<Vec<serde_json::Value>> {
        let mut query = self
            .connection
            .prepare("SELECT j.data FROM jobs j WHERE coalesce(json_extract(j.data,'$.cleared'),0)=0
                AND (coalesce(json_extract(j.data,'$.progress.status'),'queued')!='complete'
                    OR j.id IN (SELECT id FROM jobs WHERE json_extract(data,'$.progress.status')='complete' ORDER BY rowid DESC LIMIT 20))
                AND (json_extract(j.data,'$.progress.status')='complete' OR NOT EXISTS (
                    SELECT 1 FROM jobs newer WHERE newer.rowid>j.rowid
                    AND json_extract(newer.data,'$.progress.status')='complete'
                    AND json_array_length(j.data,'$.roots')>0
                    AND NOT EXISTS (
                        SELECT 1 FROM json_each(j.data,'$.roots') root
                        WHERE root.value NOT IN (SELECT value FROM json_each(newer.data,'$.roots'))
                    )
                ))
                ORDER BY j.rowid DESC")
            .map_err(|e| e.to_string())?;
        query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| parse(&r.map_err(|e| e.to_string())?))
            .collect()
    }

    fn clearable_job(&self, id: &str) -> Result<serde_json::Value> {
        self.check()?;
        let data: String = self
            .connection
            .query_row("SELECT data FROM jobs WHERE id=?1", [id], |row| row.get(0))
            .map_err(|e| format!("Import not found: {e}"))?;
        let job: serde_json::Value = parse(&data)?;
        if job["cleared"] == true || job["progress"]["status"] == "complete" {
            return Err("Only incomplete imports can be cleared.".into());
        }
        Ok(job)
    }

    fn created_import_items(&self, job: &str) -> Result<Vec<String>> {
        let mut query = self
            .connection
            .prepare_cached(
                "SELECT r.item FROM import_items r JOIN items i ON i.id=r.item
             WHERE r.job=?1 AND r.created=1 AND i.state IN ('active','undo') ORDER BY r.rowid",
            )
            .map_err(|e| e.to_string())?;
        query
            .query_map([job], |row| row.get(0))
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| e.to_string())
    }

    fn import_copy_protection(&self, job: &str, item: &Item) -> Result<Option<&'static str>> {
        let shared: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_items WHERE item=?1 AND job!=?2)",
                params![item.id, job],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if shared {
            return Ok(Some("Copies also used by another import are kept."));
        }
        if !self.companions(&item.id)?.is_empty() {
            return Ok(Some(
                "Copies with RAW/JPEG, sidecar or Live Photo companions are kept.",
            ));
        }
        let keeper: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE json_extract(data,'$.keeper')=?1 AND state IN ('active','undo'))",
            [&item.id], |row| row.get(0),
        ).map_err(|e| e.to_string())?;
        if keeper {
            return Ok(Some(
                "Copies that replaced another archived picture are kept.",
            ));
        }
        if !self
            .item_occurrences(&item.id)?
            .iter()
            .any(|source| self.cleanup_source_safe(source).is_ok())
        {
            return Ok(Some(
                "Copies without an unchanged, available source are kept.",
            ));
        }
        Ok(None)
    }

    pub fn clear_import_preview(&self, id: &str) -> Result<ClearImportPreview> {
        let job = self.clearable_job(id)?;
        let mut preview = ClearImportPreview {
            tracks_copies: job["tracksCopies"] == true,
            removable: 0,
            retained: 0,
            reasons: Vec::new(),
        };
        for item_id in self.created_import_items(id)? {
            let item = self.item(&item_id)?;
            if let Some(reason) = self.import_copy_protection(id, &item)? {
                preview.retained += 1;
                if !preview.reasons.iter().any(|existing| existing == reason) {
                    preview.reasons.push(reason.into());
                }
            } else {
                preview.removable += 1;
            }
        }
        Ok(preview)
    }

    pub fn clear_import(&mut self, job: &str, trash_copies: bool) -> Result<()> {
        self.clear_import_with(job, trash_copies, crate::recycle::recycle)
    }

    pub(crate) fn clear_import_with(
        &mut self,
        job: &str,
        trash_copies: bool,
        mut recycle: impl FnMut(&Path) -> Result<()>,
    ) -> Result<()> {
        let saved = self.clearable_job(job)?;
        if trash_copies && saved["tracksCopies"] != true {
            return Err("This older import does not track newly created copies. Clear it without removing archive files.".into());
        }
        let mut trashed = 0;
        if trash_copies {
            for id in self.created_import_items(job)? {
                let result: Result<()> = (|| {
                    let item = self.item(&id)?;
                    if self.import_copy_protection(job, &item)?.is_some() {
                        return Ok(());
                    }
                    let source = self
                        .item_occurrences(&id)?
                        .into_iter()
                        .find(|source| self.cleanup_source_safe(source).is_ok())
                        .ok_or("No unchanged source remains. The archive copy was kept.")?;
                    self.cleanup_check_cached(&source, None)?;
                    let isolated = PathBuf::from(".singulo/undo")
                        .join(format!("clear-{}", Uuid::new_v4()))
                        .join(paths::safe_name(&item.original_name));
                    self.relocate(&id, isolated, "trash-pending", item.keeper.clone())?;
                    let mut staged = self.item(&id)?;
                    let path = paths::inside(&self.root, Path::new(&staged.path))?;
                    let outcome: Result<()> = (|| {
                        self.cleanup_source_safe(&source)?;
                        let identity = media::identity(&path)?;
                        if media::hash(&path)? != staged.media.hash
                            || media::identity(&path)? != identity
                        {
                            return Err(
                                "The isolated archive copy changed; it was not trashed.".into()
                            );
                        }
                        recycle(&path)?;
                        if path.exists() {
                            return Err(
                                "The archive copy is still present; Trash did not move it.".into(),
                            );
                        }
                        Ok(())
                    })();
                    staged.state = if path.exists() { "undo" } else { "purged" }.into();
                    self.store(&staged)?;
                    outcome.map_err(|error| {
                        format!("{error} Any retained copy is in spare copies.")
                    })?;
                    trashed += 1;
                    Ok(())
                })();
                if let Err(error) = result {
                    let backup_error = match self.backup() {
                        Ok(()) => String::new(),
                        Err(error) => format!(" Recovery backup could not be updated: {error}"),
                    };
                    return Err(format!(
                        "Clearing stopped after {trashed} archive copies were moved to OS Trash: {error} Source folders were not changed. The import remains in history.{backup_error}"
                    ));
                }
            }
        }
        self.connection
            .execute(
                "UPDATE jobs SET data=json_set(data,'$.cleared',json('true')) WHERE id=?1",
                [job],
            )
            .map_err(|e| e.to_string())?;
        self.backup()
    }

    pub fn backup(&self) -> Result<()> {
        self.check()?;
        let directory = self.root.join(".singulo/recovery");
        let backup = directory.join("catalog-latest.sqlite");
        let temporary = directory.join(format!("catalog-{}.sqlite", Uuid::new_v4()));
        self.connection
            .backup(rusqlite::MAIN_DB, &temporary, None)
            .map_err(|e| e.to_string())?;
        if backup.exists() {
            fs::remove_file(&backup).map_err(|e| e.to_string())?;
        }
        fs::rename(temporary, backup).map_err(|e| e.to_string())?;
        let temporary = directory.join(format!("metadata-{}.json", Uuid::new_v4()));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let mut writer = BufWriter::new(file);
        write!(
            writer,
            "{{\"version\":1,\"archiveId\":{},\"settings\":{},\"items\":[",
            json(&self.id)?,
            json(&self.settings)?
        )
        .map_err(|e| e.to_string())?;
        for (index, table) in ["items", "occurrences"].into_iter().enumerate() {
            if index != 0 {
                writer
                    .write_all(b"],\"occurrences\":[")
                    .map_err(|e| e.to_string())?;
            }
            let mut query = self
                .connection
                .prepare(&format!("SELECT data FROM {table}"))
                .map_err(|e| e.to_string())?;
            let mut rows = query.query([]).map_err(|e| e.to_string())?;
            let mut first = true;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                if !first {
                    writer.write_all(b",").map_err(|e| e.to_string())?;
                }
                first = false;
                let data: String = row.get(0).map_err(|e| e.to_string())?;
                writer
                    .write_all(data.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
        }
        writer.write_all(b"]}").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
        writer.get_ref().sync_all().map_err(|e| e.to_string())?;
        let target = directory.join("metadata-latest.json");
        if target.exists() {
            fs::remove_file(&target).map_err(|e| e.to_string())?;
        }
        fs::rename(temporary, target).map_err(|e| e.to_string())?;
        Ok(())
    }
}
