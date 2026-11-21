use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

fn source_folder(source: &str) -> Option<&str> {
    let end = source.rfind(['/', '\\'])?;
    if end == 0 || (end == 2 && source.as_bytes().get(1) == Some(&b':')) {
        Some(&source[..=end])
    } else {
        Some(&source[..end])
    }
}

fn cache_source(source: &str) -> bool {
    source_folder(source).is_some_and(|folder| {
        folder.split(['/', '\\']).any(|name| {
            ["thumbnails", "cache", "temp"]
                .iter()
                .any(|candidate| name.eq_ignore_ascii_case(candidate))
        })
    })
}

pub(super) fn register_functions(connection: &Connection) -> Result<()> {
    let flags = rusqlite::functions::FunctionFlags::SQLITE_UTF8
        | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC;
    connection
        .create_scalar_function("source_folder", 1, flags, |context| {
            let source = context.get::<String>(0)?;
            Ok(source_folder(&source).map(str::to_owned))
        })
        .map_err(|e| e.to_string())?;
    connection
        .create_scalar_function("cache_source", 1, flags, |context| {
            let source = context.get::<String>(0)?;
            Ok(cache_source(&source))
        })
        .map_err(|e| e.to_string())
}

const CANDIDATES: &str = "
WITH source_rows AS MATERIALIZED (
    SELECT o.computer_id, coalesce(source_folder(o.source),'') AS folder,
        json_extract(o.data,'$.item') AS item,
        coalesce(json_extract(o.data,'$.computer.name'),'') AS computer_name,
        cache_source(o.source) AS named,
        CASE WHEN source_folder(o.source) IS NOT NULL
            AND json_extract(i.data,'$.media.kind')='photo'
            AND json_extract(i.data,'$.media.width') BETWEEN 1 AND 300
            AND json_extract(i.data,'$.media.height') BETWEEN 1 AND 300
            THEN 1 ELSE 0 END AS small
    FROM occurrences o JOIN items i ON i.id=json_extract(o.data,'$.item')
    WHERE json_extract(i.data,'$.media.kind') IN ('photo','heic','raw','video')
), folders AS MATERIALIZED (
    SELECT computer_id,folder,MIN(small) AS small
    FROM source_rows GROUP BY computer_id,folder
), links AS MATERIALIZED (
    SELECT s.item,s.computer_id,s.folder,MAX(s.computer_name) AS computer_name,
        MAX(s.named) AS named,f.small
    FROM source_rows s JOIN folders f ON f.computer_id=s.computer_id AND f.folder=s.folder
    GROUP BY s.item,s.computer_id,s.folder
), eligible AS (
    SELECT item FROM links GROUP BY item HAVING MIN(named OR small)=1
)
SELECT i.data,l.computer_id,l.folder,l.computer_name
FROM eligible e JOIN items i ON i.id=e.item JOIN links l ON l.item=i.id
WHERE i.state='active'
    AND NOT EXISTS (SELECT 1 FROM companions c WHERE c.owner=i.id OR c.child=i.id)
    AND NOT EXISTS (SELECT 1 FROM items other WHERE json_extract(other.data,'$.keeper')=i.id
        AND other.state IN ('active','undo'))
    AND NOT EXISTS (SELECT 1 FROM exported_xmp x WHERE x.item=i.id)
ORDER BY l.computer_id,l.folder,i.id";

type FolderKey = (String, String);

struct FolderSummary {
    folder: CatalogCleanupFolder,
    hash: blake3::Hasher,
}

impl Archive {
    fn catalog_cleanup_candidates(
        &self,
        selected: &HashSet<FolderKey>,
    ) -> Result<(Vec<CatalogCleanupFolder>, Vec<String>, usize)> {
        self.check()?;
        let mut query = self
            .connection
            .prepare(CANDIDATES)
            .map_err(|e| e.to_string())?;
        let mut rows = query.query([]).map_err(|e| e.to_string())?;
        let mut folders = BTreeMap::<FolderKey, FolderSummary>::new();
        let mut membership = HashMap::<String, (usize, usize, String)>::new();
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let data: String = row.get(0).map_err(|e| e.to_string())?;
            let item: Item = parse(&data)?;
            let computer: String = row.get(1).map_err(|e| e.to_string())?;
            let path: String = row.get(2).map_err(|e| e.to_string())?;
            let computer_name: String = row.get(3).map_err(|e| e.to_string())?;
            let key = (computer.clone(), path.clone());
            if !selected.is_empty() {
                let counts = membership.entry(item.id.clone())
                    .or_insert_with(|| (0, 0, item.path.clone()));
                counts.0 += 1;
                counts.1 += usize::from(selected.contains(&key));
            }
            let summary = folders.entry(key).or_insert_with(|| FolderSummary {
                folder: CatalogCleanupFolder {
                    computer,
                    computer_name,
                    path,
                    count: 0,
                    bytes: 0,
                    token: String::new(),
                    samples: Vec::new(),
                },
                hash: blake3::Hasher::new(),
            });
            summary.folder.count += 1;
            summary.folder.bytes = summary
                .folder
                .bytes
                .checked_add(item.media.bytes)
                .ok_or("Cleanup size exceeds the supported range.")?;
            if summary.folder.samples.len() < 3 {
                summary.folder.samples.push(item.original_name);
            }
            summary.hash.update(&(data.len() as u64).to_le_bytes());
            summary.hash.update(data.as_bytes());
        }
        let mut paths_and_ids = Vec::new();
        let mut skipped_shared = 0;
        for (id, (total, chosen, path)) in membership {
            if chosen == total && chosen > 0 {
                paths_and_ids.push((path, id));
            } else if chosen > 0 {
                skipped_shared += 1;
            }
        }
        paths_and_ids.sort_unstable();
        let ids = paths_and_ids.into_iter().map(|(_, id)| id).collect();
        Ok((
            folders
                .into_values()
                .map(|mut summary| {
                    summary.folder.token = summary.hash.finalize().to_hex().to_string();
                    summary.folder
                })
                .collect(),
            ids,
            skipped_shared,
        ))
    }

    pub fn catalog_cleanup_preview(&self) -> Result<Vec<CatalogCleanupFolder>> {
        Ok(self.catalog_cleanup_candidates(&HashSet::new())?.0)
    }

    pub fn catalog_cleanup(
        &mut self,
        selected: &[CatalogCleanupFolder],
    ) -> Result<CatalogCleanupResult> {
        self.catalog_cleanup_with(selected, crate::recycle::recycle)
    }

    pub(crate) fn catalog_cleanup_with(
        &mut self,
        selected: &[CatalogCleanupFolder],
        mut recycle: impl FnMut(&Path) -> Result<()>,
    ) -> Result<CatalogCleanupResult> {
        if selected.is_empty() {
            return Err("Select at least one folder to clean up.".into());
        }
        let keys: HashSet<_> = selected
            .iter()
            .map(|folder| (folder.computer.clone(), folder.path.clone()))
            .collect();
        if keys.len() != selected.len() {
            return Err("The cleanup selection contains duplicate folders.".into());
        }
        let (current, ids, skipped_shared) = self.catalog_cleanup_candidates(&keys)?;
        let current: BTreeMap<_, _> = current
            .into_iter()
            .map(|folder| ((folder.computer.clone(), folder.path.clone()), folder))
            .collect();
        for expected in selected {
            let actual = current
                .get(&(expected.computer.clone(), expected.path.clone()))
                .ok_or("The catalog changed. Preview the cleanup again.")?;
            if actual.token != expected.token
                || actual.count != expected.count
                || actual.bytes != expected.bytes
            {
                return Err("The catalog changed. Preview the cleanup again.".into());
            }
        }
        let mut result = CatalogCleanupResult {
            trashed: 0,
            skipped_shared,
            errors: Vec::new(),
        };
        for id in ids {
            let mut label = id.clone();
            let outcome: Result<()> = (|| {
                let item = self.item(&id)?;
                label.clone_from(&item.original_name);
                let source = paths::inside(&self.root, Path::new(&item.path))?;
                let identity = media::identity(&source)?;
                if media::hash(&source)? != item.media.hash || media::identity(&source)? != identity
                {
                    return Err("The archive file changed; it was not trashed.".into());
                }
                let isolated = PathBuf::from(".singulo/undo")
                    .join(format!("cleanup-{}", Uuid::new_v4()))
                    .join(paths::safe_name(&item.original_name));
                self.relocate(&id, isolated, "trash-pending", None)?;
                let mut staged = self.item(&id)?;
                let path = paths::inside(&self.root, Path::new(&staged.path))?;
                let outcome = (|| {
                    let identity = media::identity(&path)?;
                    if media::hash(&path)? != staged.media.hash
                        || media::identity(&path)? != identity
                    {
                        return Err("The isolated archive file changed; it was not trashed.".into());
                    }
                    recycle(&path)
                })();
                let retained = path.exists();
                staged.state = if retained { "undo" } else { "purged" }.into();
                self.transaction(|archive| {
                    archive.store(&staged)?;
                    if !retained {
                        archive
                            .connection
                            .execute("DELETE FROM source_groups WHERE item=?1", [&id])
                            .map_err(|e| e.to_string())?;
                    }
                    Ok(())
                })?;
                if !retained && let Ok(mut cache) = self.verified.lock() {
                    cache.remove(&path);
                }
                outcome
                    .map_err(|error| format!("{error} Check Trash and any retained spare copy."))?;
                if retained {
                    return Err(
                        "Trash did not move the archive file; it remains in spare copies.".into(),
                    );
                }
                Ok(())
            })();
            match outcome {
                Ok(()) => result.trashed += 1,
                Err(error) => {
                    result.errors.push(format!("{label}: {error}"));
                    break;
                }
            }
        }
        if let Err(error) = self.backup() {
            result
                .errors
                .push(format!("Catalog recovery backup failed: {error}"));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_source, source_folder};

    #[test]
    fn catalog_cleanup_recognizes_source_paths_across_platforms() {
        assert!(cache_source(r"C:\Photos\Cache\sample.jpg"));
        assert!(cache_source(r"\\server\share\TEMP\nested\file.jpg"));
        assert!(cache_source("/Photos/thumbnails/file.jpg"));
        assert!(!cache_source("/Photos/cached/file.jpg"));
        assert!(!cache_source("/Photos/cache.jpg"));
        assert_eq!(source_folder(r"C:\file.jpg"), Some(r"C:\"));
    }
}
