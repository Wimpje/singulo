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

#[derive(Serialize, Deserialize)]
struct Operation {
    item: Item,
    occurrence: Option<Occurrence>,
    stage: String,
    old_path: Option<String>,
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
        let page = archive.page("", "all", 99_900).unwrap();
        assert_eq!(page.len(), 60);
        assert_eq!(page[0].original_name, "photo-099901.jpg");
        let page_time = start.elapsed();
        assert_eq!(archive.page("photo-000001", "all", 0).unwrap().len(), 1);
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
                    .join(".photo-dedupe/recovery/metadata-latest.json"),
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

pub struct Archive {
    pub root: PathBuf,
    pub settings: Settings,
    pub id: String,
    connection: Connection,
    _lock: File,
    import_names: std::collections::HashMap<PathBuf, std::collections::HashSet<String>>,
}

fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| format!("Invalid catalog record: {e}"))
}

fn copy_new(source: &Path, destination: &Path, expected: &str) -> Result<()> {
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
    std::io::copy(&mut input, &mut output).map_err(|e| format!("Copy failed: {e}"))?;
    output
        .sync_all()
        .map_err(|e| format!("Could not flush copied file: {e}"))?;
    if media::hash(destination)? != expected {
        return Err("Copied bytes failed verification; the source is untouched.".into());
    }
    Ok(())
}

impl Archive {
    pub fn open(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            return Err("Choose an existing destination folder.".into());
        }
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let private = root.join(".photo-dedupe");
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
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS items (id TEXT PRIMARY KEY, hash TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS item_hash ON items(hash);
             CREATE TABLE IF NOT EXISTS occurrences (id TEXT PRIMARY KEY, source TEXT NOT NULL, hash TEXT NOT NULL, data TEXT NOT NULL, UNIQUE(source, hash));
             CREATE TABLE IF NOT EXISTS buckets (bucket INTEGER NOT NULL, item TEXT NOT NULL REFERENCES items(id), UNIQUE(bucket,item));
             CREATE INDEX IF NOT EXISTS bucket_lookup ON buckets(bucket);
             CREATE TABLE IF NOT EXISTS reviews (item TEXT NOT NULL, candidate TEXT NOT NULL, reason TEXT NOT NULL, UNIQUE(item,candidate));
             CREATE TABLE IF NOT EXISTS companions (owner TEXT NOT NULL, child TEXT NOT NULL, UNIQUE(owner,child));
             CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS jobs (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS occurrence_item ON occurrences(json_extract(data,'$.item'));
             CREATE INDEX IF NOT EXISTS companion_child ON companions(child);
             CREATE INDEX IF NOT EXISTS library_order ON items(state,json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'));
             CREATE INDEX IF NOT EXISTS library_visible ON items(state,json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName'))
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
        let mut archive = Self {
            root,
            settings,
            id,
            connection,
            _lock: lock,
            import_names: std::collections::HashMap::new(),
        };
        let version: u32 = archive
            .connection
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version < 2 {
            archive.transaction(|archive| {
                for occurrence in archive.occurrences()? {
                    archive.index_source(&occurrence)?;
                }
                archive
                    .connection
                    .execute_batch("PRAGMA user_version=2;")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })?;
        }
        Ok(archive)
    }

    pub fn check(&self) -> Result<()> {
        let id = fs::read_to_string(self.root.join(".photo-dedupe/archive-id"))
            .map_err(|e| format!("Selected archive is disconnected or inaccessible: {e}"))?;
        if id != self.id {
            return Err("The destination is no longer the selected archive.".into());
        }
        Ok(())
    }

    pub fn begin_import(&mut self) {
        self.import_names.clear();
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

    pub fn all(&self) -> Result<Vec<Item>> {
        self.check()?;
        let mut query = self
            .connection
            .prepare("SELECT data FROM items ORDER BY rowid DESC")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| parse(&row.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn item(&self, id: &str) -> Result<Item> {
        let data: String = self
            .connection
            .query_row("SELECT data FROM items WHERE id=?1", [id], |r| r.get(0))
            .map_err(|e| format!("Archived item not found: {e}"))?;
        parse(&data)
    }

    fn transaction<T>(&mut self, action: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
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

    pub fn page(&self, search: &str, filter: &str, offset: usize) -> Result<Vec<Item>> {
        self.check()?;
        let state = if filter == "undo" { "undo" } else { "active" };
        let condition = match filter {
            "unknown" => "AND json_extract(data,'$.media.date') IS NULL",
            "estimated" => "AND json_extract(data,'$.media.dateSource') LIKE 'estimated%'",
            "video" => "AND json_extract(data,'$.media.kind')='video'",
            _ => "",
        };
        let mut query = self.connection.prepare_cached(&format!(
            "SELECT data FROM items WHERE state=?1 AND json_extract(data,'$.media.kind')!='sidecar' {condition}
             AND (?2='' OR instr(unicode_lower(json_extract(data,'$.originalName')),?2)>0
                 OR instr(json_extract(data,'$.media.date'),?2)>0
                 OR EXISTS (SELECT 1 FROM json_each(data,'$.albums') WHERE instr(unicode_lower(value),?2)>0))
             ORDER BY json_extract(data,'$.media.date') DESC,json_extract(data,'$.originalName') LIMIT 60 OFFSET ?3"
        )).map_err(|e| e.to_string())?;
        query
            .query_map(
                params![
                    state,
                    search.to_lowercase(),
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
        source.with_extension("").to_string_lossy().into_owned()
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

    pub fn exact(&self, hash: &str) -> Result<Option<Item>> {
        let mut query = self.connection.prepare("SELECT data FROM items WHERE hash=?1 AND state!='purged' ORDER BY state='active' DESC")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([hash], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        for row in rows {
            let item: Item = parse(&row.map_err(|e| e.to_string())?)?;
            let path = paths::inside(&self.root, Path::new(&item.path))?;
            if path.is_file() && media::hash(&path)? == hash {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    pub fn occurrences(&self) -> Result<Vec<Occurrence>> {
        let mut query = self
            .connection
            .prepare("SELECT data FROM occurrences")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.map(|row| parse(&row.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn record_occurrence(&mut self, occurrence: &Occurrence) -> Result<()> {
        self.transaction(|archive| archive.record_occurrence_inner(occurrence))
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
        let mut labels = vec![occurrence.album.clone()];
        if let Some(parent) = Path::new(&occurrence.relative).parent() {
            labels.extend(parent.components().filter_map(|part| match part {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
                _ => None,
            }));
        }
        let mut changed = false;
        for label in labels {
            if !item.albums.contains(&label) {
                item.albums.push(label);
                changed = true;
            }
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
        let mut query = self.connection.prepare(
            "SELECT child FROM companions WHERE owner=?1 UNION SELECT owner FROM companions WHERE child=?1"
        ).map_err(|e| e.to_string())?;
        query
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| r.map_err(|e| e.to_string()))
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
                "SELECT r.item,r.candidate,r.reason FROM reviews r JOIN items i ON i.id=r.item
             JOIN items c ON c.id=r.candidate WHERE i.state='active' AND c.state='active'",
            )
            .map_err(|e| e.to_string())?;
        query.query_map([], |r| Ok(serde_json::json!({
            "item": r.get::<_,String>(0)?, "candidate": r.get::<_,String>(1)?, "reason": r.get::<_,String>(2)?
        }))).map_err(|e| e.to_string())?.map(|r| r.map_err(|e| e.to_string())).collect()
    }

    pub fn dismiss_review(&mut self, item: &str, candidate: &str) -> Result<()> {
        self.connection.execute("DELETE FROM reviews WHERE (item=?1 AND candidate=?2) OR (item=?2 AND candidate=?1)",
            params![item, candidate]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn place(
        &mut self,
        source: &Path,
        item: Item,
        occurrence: Option<Occurrence>,
        old_path: Option<String>,
    ) -> Result<()> {
        self.check()?;
        let stage_relative = format!(".photo-dedupe/staging/{}", Uuid::new_v4());
        let stage = paths::inside(&self.root, Path::new(&stage_relative))?;
        copy_new(source, &stage, &item.media.hash)?;
        let operation = Operation {
            item,
            occurrence,
            stage: stage_relative,
            old_path,
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
            Ok(())
        })?;
        if let Some(old) = &operation.old_path {
            let old = paths::inside(&self.root, Path::new(old))?;
            if old != target && old.exists() {
                if media::hash(&old)? != operation.item.media.hash {
                    return Err(format!(
                        "Original archive path changed during relocation: {}",
                        old.display()
                    ));
                }
                fs::remove_file(old).map_err(|e| e.to_string())?;
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
        for mut occurrence in self.occurrences()? {
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
        for mut item in self.all()? {
            if item.state == "purge-pending" {
                let path = paths::inside(&self.root, Path::new(&item.path))?;
                item.state = if path.exists() { "undo" } else { "purged" }.into();
                self.store(&item)?;
                errors.push(format!("Reconciled interrupted archive purge for {}. Retained files require a new confirmation.", item.original_name));
            }
        }
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
        let path = paths::unique(&self.root, &wanted, &Uuid::new_v4().to_string())?;
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
        if keep.state != "active" || media::hash(&keep_path)? != keep.media.hash {
            return Err("The chosen keeper is not a verified active copy.".into());
        }
        if !self.companions(id)?.is_empty() {
            return Err("This item has RAW/JPEG, sidecar or Live Photo companions. Keep the set together; automatic removal is disabled.".into());
        }
        let item = self.item(id)?;
        let wanted = PathBuf::from(".photo-dedupe/undo")
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

    pub fn cleanup_rows(&self) -> Result<Vec<CleanupRow>> {
        self.check()?;
        self.occurrences()?
            .into_iter()
            .filter(|o| o.status == "pending")
            .map(|o| {
                let check = self.cleanup_check(&o);
                Ok(CleanupRow {
                    id: o.id,
                    source: o.source,
                    bytes: o.bytes,
                    eligible: check.is_ok(),
                    reason: check
                        .err()
                        .unwrap_or_else(|| "Byte-identical archived original verified.".into()),
                })
            })
            .collect()
    }

    pub fn cleanup_check(&self, occurrence: &Occurrence) -> Result<()> {
        self.check()?;
        let source = Path::new(&occurrence.source);
        let root = Path::new(&occurrence.root)
            .canonicalize()
            .map_err(|e| format!("Source root unavailable: {e}"))?;
        let canonical = source
            .canonicalize()
            .map_err(|e| format!("Source unavailable: {e}"))?;
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
        if media::hash(source)? != occurrence.hash {
            return Err("Source content no longer matches the imported original.".into());
        }
        if self.exact(&occurrence.hash)?.is_none() {
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
        let mut occurrence = self
            .occurrences()?
            .into_iter()
            .find(|o| o.id == id)
            .ok_or("Source occurrence not found.")?;
        if occurrence.status != "pending" {
            return Err("Source is not eligible for a new cleanup operation.".into());
        }
        self.cleanup_check(&occurrence)?;
        let source = PathBuf::from(&occurrence.source);
        let parent = source.parent().ok_or("Missing source parent.")?;
        let private = parent.join(format!(".photo-dedupe-cleanup-{}", Uuid::new_v4()));
        fs::create_dir(&private).map_err(|e| format!("Cannot safely stage source cleanup: {e}"))?;
        let stage = private.join(source.file_name().ok_or("Missing filename.")?);
        occurrence.status = format!("staged:{}", stage.display());
        self.record_occurrence(&occurrence)?;
        fs::rename(&source, &stage)
            .map_err(|e| format!("Source staging failed; it was not trashed: {e}"))?;
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
        let occurrences = self.occurrences()?;
        let mut directories = Vec::new();
        for occurrence in occurrences.iter().filter(|o| o.status == "trashed") {
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
        self.backup()?;
        item.state = "purge-pending".into();
        self.store(&item)?;
        if let Err(error) = fs::remove_file(path) {
            item.state = "undo".into();
            self.store(&item)?;
            return Err(format!("Undo original was not purged: {error}"));
        }
        item.state = "purged".into();
        self.store(&item)?;
        self.backup()
    }

    pub fn move_preview(&self, ids: &[String]) -> Result<Vec<MoveRow>> {
        let mut result = Vec::new();
        let mut planned = std::collections::HashSet::new();
        let mut ids = ids.to_vec();
        for id in ids.clone() {
            for companion in self.companions(&id)? {
                if self.item(&companion)?.media.kind == "sidecar" && !ids.contains(&companion) {
                    ids.push(companion);
                }
            }
        }
        for id in &ids {
            let item = self.item(id)?;
            if item.state != "active" {
                continue;
            }
            let wanted = paths::relative(&self.settings, &item.media, &item.original_name)?;
            if wanted.to_string_lossy().replace('\\', "/") == item.path {
                continue;
            }
            let mut to = paths::unique(&self.root, &wanted, id)?;
            if !planned.insert(to.to_string_lossy().to_lowercase()) {
                to = wanted.with_file_name(format!(
                    "{}__{}",
                    id,
                    paths::safe_name(&item.original_name)
                ));
                if !planned.insert(to.to_string_lossy().to_lowercase()) {
                    return Err("Reorganization has an unresolved collision.".into());
                }
            }
            result.push(MoveRow {
                id: id.clone(),
                from: item.path,
                to: to.to_string_lossy().replace('\\', "/"),
            });
        }
        Ok(result)
    }

    pub fn reorganize(&mut self, preview: &[MoveRow]) -> Result<()> {
        self.check()?;
        self.backup()?;
        let manifest = self.root.join(".photo-dedupe/recovery").join(format!(
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
        for row in preview {
            let mut item = self.item(&row.id)?;
            if item.path != row.from {
                return Err(
                    "Library changed since the reorganization preview; preview again.".into(),
                );
            }
            if paths::inside(&self.root, Path::new(&row.to))?.exists() {
                return Err("A target appeared after preview; preview again.".into());
            }
            let source = paths::inside(&self.root, Path::new(&row.from))?;
            if media::hash(&source)? != item.media.hash {
                return Err("Archived original changed since import.".into());
            }
            item.path = row.to.clone();
            item.settings = self.settings.clone();
            self.place(&source, item, None, Some(row.from.clone()))?;
        }
        self.backup()
    }

    fn latest_reorganization(&self) -> Result<(PathBuf, Reorganization)> {
        let mut files = fs::read_dir(self.root.join(".photo-dedupe/recovery"))
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
        let mut result = Vec::new();
        for row in record.moves.iter().rev() {
            let current = self.item(&row.id)?;
            if current.path == row.from {
                continue;
            }
            if current.path != row.to || current.state != "active" {
                return Err("A reorganized item has changed since the operation. Restore conflicts manually rather than overwriting.".into());
            }
            if paths::inside(&self.root, Path::new(&row.from))?.exists() {
                return Err(format!(
                    "Rollback conflict at {}. Nothing will be overwritten.",
                    row.from
                ));
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
        self.backup()?;
        for row in &expected {
            let mut item = self.item(&row.id)?;
            let source = paths::inside(&self.root, Path::new(&row.from))?;
            item.path = row.to.clone();
            item.settings = record
                .originals
                .iter()
                .find(|i| i.id == row.id)
                .ok_or("Missing original layout in recovery manifest.")?
                .settings
                .clone();
            self.place(&source, item, None, Some(row.from.clone()))?;
        }
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
        let mut item = self.item(id)?;
        if item.state != "active" {
            return Err("Restore this original before correcting its date.".into());
        }
        let selected =
            media::valid_date(date).ok_or("Enter a valid non-default date, not in the future.")?;
        let old = item.path.clone();
        let source = paths::inside(&self.root, Path::new(&old))?;
        item.media.date = Some(selected);
        item.media.date_source = "manual".into();
        let target = paths::relative(&item.settings, &item.media, &item.original_name)?;
        item.path = paths::unique(&self.root, &target, id)?
            .to_string_lossy()
            .replace('\\', "/");
        self.place(&source, item, None, Some(old))?;
        for companion in self.companions(id)? {
            if self.item(&companion)?.media.kind == "sidecar" {
                self.correct_date(&companion, date)?;
            }
        }
        self.backup()
    }

    pub fn save_job(&self, id: &str, data: &serde_json::Value) -> Result<()> {
        self.connection
            .execute(
                "INSERT OR REPLACE INTO jobs VALUES(?1,?2)",
                params![id, json(data)?],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn jobs(&self) -> Result<Vec<serde_json::Value>> {
        let mut query = self
            .connection
            .prepare("SELECT data FROM jobs ORDER BY rowid DESC LIMIT 20")
            .map_err(|e| e.to_string())?;
        query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|r| parse(&r.map_err(|e| e.to_string())?))
            .collect()
    }

    pub fn backup(&self) -> Result<()> {
        self.check()?;
        let directory = self.root.join(".photo-dedupe/recovery");
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
