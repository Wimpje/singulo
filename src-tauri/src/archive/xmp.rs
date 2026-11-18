//! Managed exports are separate from imported companions. A durable intent precedes
//! every filesystem change; replaced bytes stay in recovery until publication commits.
use super::{Archive, copy_new, json, parse};
use crate::{media, model::*, paths};
use roxmltree::{Document, Node};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    ops::Range,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC: &str = "http://purl.org/dc/elements/1.1/";
const LR: &str = "http://ns.adobe.com/lightroom/1.0/";
const DIGIKAM: &str = "http://www.digikam.org/ns/1.0/";
const XMP: &str = "http://ns.adobe.com/xap/1.0/";
const EXIF: &str = "http://ns.adobe.com/exif/1.0/";
const PS: &str = "http://ns.adobe.com/photoshop/1.0/";
const EXIF_EX: &str = "http://cipa.jp/exif/1.0/";
// Immich's documented sidecar date precedence includes SubSec* aliases before
// DateTimeOriginal/CreateDate. Strip conflicting aliases only in the export.
// https://docs.immich.app/features/xmp-sidecars/#metadata-fields
const DATE_PROPERTIES: &[(&str, &str)] = &[
    (EXIF, "SubSecDateTimeOriginal"),
    (EXIF, "DateTimeOriginal"),
    (XMP, "SubSecCreateDate"),
    (XMP, "CreateDate"),
    (XMP, "CreationDate"),
    (XMP, "MediaCreateDate"),
    (XMP, "SubSecMediaCreateDate"),
    (XMP, "DateTimeCreated"),
    (PS, "DateCreated"),
    (EXIF, "SubSecTimeOriginal"),
    (EXIF, "OffsetTimeOriginal"),
    (EXIF_EX, "OffsetTimeOriginal"),
    (EXIF, "TimeZoneOffset"),
    (EXIF, "SubSecTimeDigitized"),
    (EXIF, "OffsetTimeDigitized"),
    (EXIF_EX, "OffsetTimeDigitized"),
];
const OWNER: &str = "https://singulo.local/ns/xmp/1.0/";
const EMPTY: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"></rdf:RDF></x:xmpmeta>";
const MAX_XMP: u64 = 8 * 1024 * 1024;
pub(super) type DirectoryNames = HashMap<PathBuf, HashMap<String, Vec<OsString>>>;

#[derive(Default, Serialize)]
pub struct XmpSummary {
    pub created: usize,
    pub updated: usize,
    pub removed: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Export {
    item: String,
    path: String,
    hash: String,
    base: String,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    old: Option<Export>,
    new: Option<Export>,
    stage: Option<String>,
    parked: String,
    committed: bool,
    #[serde(default)]
    discarded: Option<Export>,
}

fn escaped(value: &str) -> Result<String> {
    if value.chars().any(|c| {
        !matches!(c, '\t' | '\n' | '\r') && (c < '\u{20}' || matches!(c, '\u{fffe}' | '\u{ffff}'))
    }) {
        return Err("Metadata contains characters that cannot be represented in XML.".into());
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn named(node: Node<'_, '_>, namespace: &str, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some(namespace)
        && node.tag_name().name() == name
}

fn primary_description(node: Node<'_, '_>) -> bool {
    named(node, RDF, "Description")
        && node.parent().is_some_and(|p| named(p, RDF, "RDF"))
        && node.attribute((RDF, "about")).unwrap_or("").is_empty()
        && node.attribute((RDF, "nodeID")).is_none()
        && node.attribute((RDF, "ID")).is_none()
}

fn digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn document(text: &str) -> Result<Document<'_>> {
    let doc =
        Document::parse(text).map_err(|e| format!("Cannot safely merge this XMP document: {e}"))?;
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if start.starts_with("<?xml")
        && let Some(declaration) = start.split("?>").next()
        && let Some((_, encoding)) = declaration.split_once("encoding")
    {
        let value = encoding
            .trim_start()
            .strip_prefix('=')
            .ok_or("Invalid XML encoding declaration.")?
            .trim_start();
        let quote = value.chars().next().ok_or("Missing XML encoding.")?;
        let name = value[quote.len_utf8()..].split(quote).next().unwrap_or("");
        if !name.eq_ignore_ascii_case("utf-8") {
            return Err("Only UTF-8 XMP can be merged without changing its encoding.".into());
        }
    }
    Ok(doc)
}

fn scalar(node: Node<'_, '_>) -> Result<String> {
    if node.attributes().len() != 0 || node.children().any(|n| !n.is_text()) {
        return Err("An XMP property has qualifiers or structured content; it was preserved without exporting over it.".into());
    }
    Ok(node.children().filter_map(|n| n.text()).collect())
}

fn keyword_property(namespace: &str, name: &str, values: &[String]) -> Result<String> {
    let mut result = format!("<p:{name} xmlns:p=\"{namespace}\" xmlns:rdf=\"{RDF}\"><rdf:Bag>");
    for value in values {
        result.push_str(&format!("<rdf:li>{}</rdf:li>", escaped(value)?));
    }
    result.push_str(&format!("</rdf:Bag></p:{name}>"));
    Ok(result)
}

// Edit only understood properties; all other source XML is retained verbatim,
// including unknown namespaces, attributes and structured metadata.
fn render(base: &str, archive: &str, item: &Item) -> Result<String> {
    let doc = document(base)?;
    let manual_date = item
        .media
        .date
        .as_deref()
        .filter(|_| item.media.date_source == "manual");
    let rdf: Vec<_> = doc
        .descendants()
        .filter(|n| named(*n, RDF, "RDF"))
        .collect();
    if rdf.len() != 1
        || !doc.descendants().any(|n| {
            n == doc.root_element()
                && (named(n, "adobe:ns:meta/", "xmpmeta") || named(n, RDF, "RDF"))
        })
    {
        return Err(
            "XMP must contain one RDF metadata packet; the existing sidecar was left untouched."
                .into(),
        );
    }
    let rdf = rdf[0];
    if doc.descendants().any(|n| {
        n.is_element()
            && (n.tag_name().namespace() == Some(OWNER)
                || n.attributes().any(|a| a.namespace() == Some(OWNER)))
    }) {
        return Err(
            "The fallback contains an ownership marker; it cannot safely become a new export."
                .into(),
        );
    }
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut keywords = Vec::new();
    let mut hierarchical = Vec::new();
    let mut digikam = Vec::new();
    let mut has_digikam = false;
    for node in rdf.descendants().filter(|n| n.is_element()) {
        let primary = primary_description(node);
        let property = node.parent().is_some_and(primary_description);
        if node.tag_name().name() == "Keywords"
            || node.attributes().any(|a| a.name() == "Keywords")
        {
            return Err("An unsupported explicit XMP Keywords property could be shadowed; export was skipped without changing the fallback.".into());
        }
        let is_keywords = [(DC, "subject"), (LR, "hierarchicalSubject"), (DIGIKAM, "TagsList")]
            .iter()
            .any(|(ns, name)| named(node, ns, name));
        if is_keywords && !property {
            return Err("A non-primary or nested XMP keyword collection cannot be safely merged; export was skipped.".into());
        }
        if node
            .attributes()
            .any(|a| {
                [(DC, "subject"), (LR, "hierarchicalSubject"), (DIGIKAM, "TagsList")]
                    .iter()
                    .any(|(ns, name)| a.namespace() == Some(*ns) && a.name() == *name)
            })
        {
            return Err(
                "A non-array XMP keyword collection cannot be safely merged; export was skipped.".into(),
            );
        }
        let is_date = DATE_PROPERTIES
            .iter()
            .any(|(ns, name)| named(node, ns, name));
        if manual_date.is_some() && is_date && !property {
            return Err("A non-primary or nested XMP date could override the correction; the fallback was left untouched.".into());
        }
        if manual_date.is_some()
            && !primary
            && node.attributes().any(|a| {
                DATE_PROPERTIES
                    .iter()
                    .any(|(ns, name)| a.namespace() == Some(*ns) && a.name() == *name)
            })
        {
            return Err("A non-primary XMP date attribute could override the correction; export was skipped.".into());
        }
        if !primary && !property {
            continue;
        }
        if is_date && manual_date.is_some() {
            scalar(node)?;
            edits.push((node.range(), String::new()));
        }
        for attribute in node.attributes() {
            if manual_date.is_some()
                && DATE_PROPERTIES.iter().any(|(ns, name)| {
                    attribute.namespace() == Some(*ns) && attribute.name() == *name
                })
            {
                if !primary {
                    return Err("A nested XMP date attribute cannot be safely replaced.".into());
                }
                edits.push((attribute.range(), String::new()));
            }
        }
        let values = if named(node, DC, "subject") {
            &mut keywords
        } else if named(node, LR, "hierarchicalSubject") {
            &mut hierarchical
        } else if named(node, DIGIKAM, "TagsList") {
            has_digikam = true;
            &mut digikam
        } else {
            continue;
        };
        if !property
            || node.attributes().len() != 0
            || node.children().any(|n| {
                !n.is_element() && (!n.is_text() || !n.text().unwrap_or("").trim().is_empty())
            })
        {
            return Err("A qualified XMP keyword collection cannot be safely merged.".into());
        }
        let children: Vec<_> = node.children().filter(|n| n.is_element()).collect();
        if children.len() != 1
            || !named(children[0], RDF, "Bag")
            || children[0].attributes().len() != 0
        {
            return Err("An unsupported XMP keyword collection was left untouched.".into());
        }
        for entry in children[0].children() {
            if entry.is_text() && entry.text().unwrap_or("").trim().is_empty() {
                continue;
            }
            if !named(entry, RDF, "li") {
                return Err(
                    "An XMP keyword collection contains unknown fields; export was skipped.".into(),
                );
            }
            let value = scalar(entry)?;
            if !values.contains(&value) {
                values.push(value);
            }
        }
        edits.push((node.range(), String::new()));
    }
    if has_digikam {
        // Match Immich's interpretation of Lightroom hierarchy separators
        // when preserving those values in its higher-priority DigiKam list.
        for value in &hierarchical {
            let value = value
                .split('|')
                .map(|part| part.replace('/', "|"))
                .collect::<Vec<_>>()
                .join("/");
            if !digikam.contains(&value) {
                digikam.push(value);
            }
        }
    }
    // dc:subject is the standard XMP mapping for IPTC keywords. Promoting it
    // must not reinterpret a flat keyword as Lightroom's hierarchy syntax.
    for keyword in &keywords {
        if !has_digikam && keyword.contains(['|', '/']) {
            return Err("A flat XMP keyword contains hierarchy separators and cannot be safely promoted to Lightroom tags; export was skipped.".into());
        }
        if !hierarchical.contains(keyword) {
            hierarchical.push(keyword.clone());
        }
        if has_digikam && !digikam.contains(keyword) {
            digikam.push(keyword.clone());
        }
    }
    for album in &item.albums {
        if !keywords.contains(album) {
            keywords.push(album.clone());
        }
        if !hierarchical.contains(album) {
            hierarchical.push(album.clone());
        }
        if has_digikam && !digikam.contains(album) {
            digikam.push(album.clone());
        }
    }
    let mut addition = format!(
        "<rdf:Description xmlns:rdf=\"{RDF}\" rdf:about=\"\" xmlns:singulo=\"{OWNER}\" singulo:Archive=\"{}\" singulo:Item=\"{}\" singulo:OriginalHash=\"{}\">",
        escaped(archive)?,
        escaped(&item.id)?,
        escaped(&item.media.hash)?
    );
    if let Some(date) = manual_date {
        // A day-only correction stays day-only; never invent midnight,
        // fractional seconds, UTC or an offset absent from the chosen value.
        for (ns, name) in [(EXIF, "DateTimeOriginal"), (PS, "DateCreated")] {
            addition.push_str(&format!(
                "<p:{name} xmlns:p=\"{ns}\">{}</p:{name}>",
                escaped(date)?
            ));
        }
    }
    addition.push_str(&keyword_property(DC, "subject", &keywords)?);
    addition.push_str(&keyword_property(LR, "hierarchicalSubject", &hierarchical)?);
    if has_digikam {
        addition.push_str(&keyword_property(DIGIKAM, "TagsList", &digikam)?);
    }
    addition.push_str("</rdf:Description>");
    let range = rdf.range();
    let source = &base[range.clone()];
    if source.trim_end().ends_with("/>") {
        let end = range.end - 2;
        let name = source[1..]
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .ok_or("Missing RDF tag name.")?;
        edits.push((end..range.end, format!(">{addition}</{name}>")));
    } else {
        let end = range.start + source.rfind("</").ok_or("Missing RDF closing tag.")?;
        edits.push((end..end, addition));
    }
    edits.sort_by_key(|(range, _)| range.start);
    if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return Err("Overlapping XMP properties cannot be safely merged.".into());
    }
    let mut output = base.to_owned();
    for (range, value) in edits.into_iter().rev() {
        output.replace_range(range, &value);
    }
    document(&output)?;
    Ok(output)
}

impl Archive {
    // The caller must retain the application's busy guard and the primary
    // Archive (which owns writer.lock). Browsing keeps using its own connection.
    pub fn xmp_worker(&self) -> Result<Self> {
        self.check()?;
        let catalog = paths::inside(&self.root, Path::new(".singulo/catalog.sqlite"))?;
        let connection = rusqlite::Connection::open_with_flags(
            catalog,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|e| e.to_string())?;
        connection
            .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
            .map_err(|e| e.to_string())?;
        Ok(Self {
            root: self.root.clone(),
            settings: self.settings.clone(),
            id: self.id.clone(),
            connection,
            _lock: None,
            import_names: Default::default(),
            verified: self.verified.clone(),
            xmp_names: Default::default(),
            xmp_work: self.xmp_work.clone(),
            xmp_exporting: self.xmp_exporting.clone(),
        })
    }

    pub fn xmp_worker_if_needed(&self) -> Result<Option<Self>> {
        // After opting out, retry removals only on explicit removal, not during
        // unrelated changes/imports. Startup reports the work still pending.
        if !self.xmp_exporting.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(None);
        }
        self.xmp_worker().map(Some)
    }

    fn xmp_mode(&self) -> Result<String> {
        self.connection
            .query_row(
                "SELECT value FROM config WHERE key='xmp-enabled'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map(|v| v.unwrap_or_default())
            .map_err(|e| e.to_string())
    }

    fn xmp_record(&self, item: &str) -> Result<Option<Export>> {
        self.connection
            .query_row("SELECT data FROM exported_xmp WHERE item=?1", [item], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .as_deref()
            .map(parse)
            .transpose()
    }

    fn xmp_attention(&self, id: &str, message: impl std::fmt::Display) -> String {
        let path = self
            .connection
            .query_row(
                "SELECT json_extract(data,'$.path') FROM items WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                self.xmp_record(id).ok().flatten().map(|record| {
                    record
                        .path
                        .strip_suffix(".xmp")
                        .unwrap_or(&record.path)
                        .to_owned()
                })
            })
            .or_else(|| {
                self.connection.query_row(
                "SELECT coalesce(json_extract(data,'$.new.path'),json_extract(data,'$.old.path'),
                     json_extract(data,'$.discarded.path')) FROM xmp_operations WHERE item=?1",
                [id], |r| r.get::<_, String>(0)
            ).optional().ok().flatten().map(|path| {
                path.strip_suffix(".xmp").unwrap_or(&path).to_owned()
            })
            })
            .unwrap_or_else(|| format!("Item {id} (archive path unavailable)"));
        format!("{path}: {message}")
    }

    pub fn xmp_recovery_attention(&self) -> Result<Vec<String>> {
        // Opening only inspects intent markers. Replaying a 100k-file queue must
        // wait for an explicit worker operation after the archive mutex is free.
        if !self.xmp_work.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let pending: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM xmp_operations)
                 OR EXISTS(SELECT 1 FROM xmp_dirty)
                 OR (EXISTS(SELECT 1 FROM exported_xmp) AND (
                     EXISTS(SELECT 1 FROM config WHERE key='xmp-enabled' AND value='removing')
                     OR NOT EXISTS(SELECT 1 FROM config WHERE key='xmp-policy' AND value='manual-active-keywords-v3')))",
            [], |r| r.get(0)
        ).map_err(|e| e.to_string())?;
        if !pending {
            return Ok(Vec::new());
        }
        let resume = if self.xmp_mode()? == "removing" {
            "Remove exported XMP"
        } else {
            "Export XMP"
        };
        Ok(vec![format!(
            "Managed XMP sidecar work is pending. It was not replayed while opening the archive, so browsing need not wait for the export queue. Staged and recovery bytes were retained. Run {resume} to resume and see any per-file problems."
        )])
    }

    fn read_xmp(&self, relative: &str) -> Result<String> {
        let path = paths::inside(&self.root, Path::new(relative))?;
        let identity = media::identity(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_XMP {
            return Err("Sidecar is not a regular XMP file smaller than 8 MB.".into());
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .map_err(|e| e.to_string())?
            .take(MAX_XMP + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        paths::inside(&self.root, Path::new(relative))?;
        if bytes.len() as u64 != metadata.len() || media::identity(&path)? != identity {
            return Err("Sidecar changed while being read; it was left untouched.".into());
        }
        String::from_utf8(bytes)
            .map_err(|_| "Only UTF-8 XMP can be merged without losing fields.".into())
    }

    fn verify_xmp(&self, record: &Export, relative: &str) -> Result<()> {
        let text = self.read_xmp(relative)?;
        if digest(text.as_bytes()) != record.hash {
            return Err(format!("Modified sidecar was kept: {relative}"));
        }
        let doc = document(&text)?;
        let owners: Vec<_> = doc
            .descendants()
            .filter(|n| named(*n, RDF, "Description") && n.attribute((OWNER, "Archive")).is_some())
            .collect();
        if owners.len() != 1
            || owners[0].attribute((OWNER, "Archive")) != Some(self.id.as_str())
            || owners[0].attribute((OWNER, "Item")) != Some(record.item.as_str())
        {
            return Err(format!(
                "Sidecar ownership could not be verified: {relative}"
            ));
        }
        Ok(())
    }

    fn xmp_exists(&self, relative: &str) -> Result<bool> {
        let target = paths::inside(&self.root, Path::new(relative))?;
        match fs::symlink_metadata(&target) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn xmp_collision(&self, relative: &str) -> Result<bool> {
        let target = paths::inside(&self.root, Path::new(relative))?;
        paths::component(
            &target
                .file_name()
                .ok_or("Missing sidecar filename.")?
                .to_string_lossy(),
        )?;
        let parent = target.parent().ok_or("Missing sidecar folder.")?;
        if !parent.exists() {
            return Ok(false);
        }
        let wanted = target
            .file_name()
            .ok_or("Missing sidecar filename.")?
            .to_string_lossy();
        let mut directories = self.xmp_names.lock().map_err(|e| e.to_string())?;
        if !directories.contains_key(parent) {
            let mut names: HashMap<String, Vec<OsString>> = HashMap::new();
            for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let name = entry.file_name();
                names
                    .entry(name.to_string_lossy().to_ascii_lowercase())
                    .or_default()
                    .push(name);
            }
            directories.insert(parent.to_path_buf(), names);
        }
        let matches = directories
            .get(parent)
            .and_then(|names| names.get(&wanted.to_ascii_lowercase()));
        if matches.is_some_and(|names| names.iter().any(|name| name != target.file_name().unwrap()))
        {
            return Err(format!(
                "A case-colliding sidecar name was kept beside {relative}"
            ));
        }
        Ok(matches.is_some_and(|names| !names.is_empty()))
    }

    fn xmp_name_changed(&self, relative: &str, present: bool) -> Result<()> {
        let target = paths::inside(&self.root, Path::new(relative))?;
        let parent = target.parent().ok_or("Missing sidecar folder.")?;
        let name = target
            .file_name()
            .ok_or("Missing sidecar filename.")?
            .to_owned();
        if let Some(names) = self
            .xmp_names
            .lock()
            .map_err(|e| e.to_string())?
            .get_mut(parent)
        {
            let key = name.to_string_lossy().to_ascii_lowercase();
            let matches = names.entry(key).or_default();
            matches.retain(|entry| entry != &name);
            if present {
                matches.push(name);
            }
        }
        Ok(())
    }

    fn flush_xmp_folder(&self, relative: &str) -> Result<()> {
        #[cfg(unix)]
        {
            let path = paths::inside(&self.root, Path::new(relative))?;
            File::open(path.parent().ok_or("Missing XMP folder.")?)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        paths::inside(&self.root, Path::new(relative))?;
        Ok(())
    }

    fn save_xmp_pending(&self, item: &str, pending: &Pending) -> Result<()> {
        self.connection.execute(
            "INSERT INTO xmp_operations VALUES(?1,?2) ON CONFLICT(item) DO UPDATE SET data=excluded.data",
            params![item, json(pending)?]
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn finish_xmp(
        &self,
        item: &str,
        pending: &mut Pending,
        summary: &mut XmpSummary,
    ) -> Result<()> {
        self.check()?;
        if !pending.committed
            && let Some(new) = &pending.new
        {
            let owner = self.item(item)?;
            let mode = self.xmp_mode()?;
            let publish = mode == "true"
                && owner.state == "active"
                && !owner.path.starts_with(".singulo/staging/")
                && new.path == format!("{}.xmp", owner.path);
            if !publish {
                if mode == "true" {
                    self.connection
                        .execute(
                            "INSERT INTO xmp_dirty VALUES(?1) ON CONFLICT(item) DO NOTHING",
                            [item],
                        )
                        .map_err(|e| e.to_string())?;
                }
                let present = self.xmp_exists(&new.path)?;
                let already_published = present && self.verify_xmp(new, &new.path).is_ok();
                if !already_published {
                    if present
                        && !pending.old.as_ref().is_some_and(|old| {
                            old.path == new.path && self.verify_xmp(old, &old.path).is_ok()
                        })
                    {
                        return Err(format!(
                            "A conflicting sidecar prevented cancellation of an outdated export: {}",
                            new.path
                        ));
                    }
                    if mode == "true" && owner.state != "purged" {
                        self.connection.execute(
                            "INSERT INTO xmp_baselines VALUES(?1,?2) ON CONFLICT(item) DO UPDATE SET base=excluded.base",
                            params![item, new.base]
                        ).map_err(|e| e.to_string())?;
                    }
                    pending.discarded = pending.new.take();
                    self.save_xmp_pending(item, pending)?;
                }
            }
        }
        if !pending.committed {
            if let Some(new) = &pending.new
                && self.xmp_collision(&new.path)?
                && !(pending.old.as_ref().is_some_and(|old| old.path == new.path)
                    && self
                        .verify_xmp(pending.old.as_ref().unwrap(), &new.path)
                        .is_ok())
                && self.verify_xmp(new, &new.path).is_err()
            {
                return Err(format!(
                    "Sidecar name is occupied; nothing was overwritten: {}",
                    new.path
                ));
            }
            if let Some(old) = &pending.old {
                let already_published = pending.new.as_ref().is_some_and(|new| {
                    new.path == old.path && self.verify_xmp(new, &new.path).is_ok()
                });
                if self.xmp_exists(&pending.parked)? {
                    self.verify_xmp(old, &pending.parked)?;
                    if self.xmp_exists(&old.path)? && !already_published {
                        return Err(format!(
                            "A file appeared at {}; recovery copy was retained.",
                            old.path
                        ));
                    }
                } else if !already_published {
                    self.verify_xmp(old, &old.path)?;
                    self.check()?;
                    let source = paths::inside(&self.root, Path::new(&old.path))?;
                    let parked = paths::inside(&self.root, Path::new(&pending.parked))?;
                    fs::rename(source, parked)
                        .map_err(|e| format!("Cannot retain previous XMP bytes: {e}"))?;
                    self.xmp_name_changed(&old.path, false)?;
                    self.flush_xmp_folder(&old.path)?;
                    self.flush_xmp_folder(&pending.parked)?;
                    self.verify_xmp(old, &pending.parked)?;
                }
            }
            if let Some(new) = &pending.new {
                if !self.xmp_exists(&new.path)? {
                    if self.xmp_collision(&new.path)? {
                        return Err(format!("Sidecar name is occupied: {}", new.path));
                    }
                    let stage = pending
                        .stage
                        .as_deref()
                        .ok_or("Missing XMP staging record.")?;
                    self.verify_xmp(new, stage)?;
                    self.check()?;
                    let target = paths::inside(&self.root, Path::new(&new.path))?;
                    fs::create_dir_all(target.parent().ok_or("Missing sidecar folder.")?)
                        .map_err(|e| e.to_string())?;
                    paths::inside(&self.root, Path::new(&new.path))?;
                    copy_new(
                        &paths::inside(&self.root, Path::new(stage))?,
                        &target,
                        &new.hash,
                    )?;
                    self.xmp_name_changed(&new.path, true)?;
                    self.flush_xmp_folder(&new.path)?;
                }
                self.verify_xmp(new, &new.path)?;
                if pending.old.is_some() {
                    summary.updated += 1;
                } else {
                    summary.created += 1;
                }
            } else if pending.old.is_some() {
                summary.removed += 1;
            }
            pending.committed = true;
            let tx = self
                .connection
                .unchecked_transaction()
                .map_err(|e| e.to_string())?;
            if let Some(new) = &pending.new {
                tx.execute(
                    "INSERT INTO exported_xmp VALUES(?1,?2) ON CONFLICT(item) DO UPDATE SET data=excluded.data",
                    params![item, json(new)?]
                ).map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM xmp_baselines WHERE item=?1", [item])
                    .map_err(|e| e.to_string())?;
            } else {
                tx.execute("DELETE FROM exported_xmp WHERE item=?1", [item])
                    .map_err(|e| e.to_string())?;
            }
            tx.execute(
                "UPDATE xmp_operations SET data=?1 WHERE item=?2",
                params![json(pending)?, item],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
        }
        if let Some(new) = &pending.new {
            self.verify_xmp(new, &new.path)?;
        }
        for (record, path) in [
            (pending.old.as_ref(), Some(pending.parked.as_str())),
            (
                pending.new.as_ref().or(pending.discarded.as_ref()),
                pending.stage.as_deref(),
            ),
        ] {
            if let (Some(record), Some(path)) = (record, path)
                && self.xmp_exists(path)?
            {
                self.verify_xmp(record, path)?;
                self.check()?;
                fs::remove_file(paths::inside(&self.root, Path::new(path))?)
                    .map_err(|e| format!("Cannot clean verified XMP recovery file {path}: {e}"))?;
                self.flush_xmp_folder(path)?;
            }
        }
        let parked = Path::new(&pending.parked);
        if let Some(parent) = parked.parent()
            && parent.parent() == Some(Path::new(".singulo/recovery"))
            && parent
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("xmp-"))
        {
            let directory = paths::inside(&self.root, parent)?;
            match fs::remove_dir(directory) {
                Ok(()) => self.flush_xmp_folder(&parent.to_string_lossy())?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("XMP recovery folder was retained: {error}")),
            }
        }
        self.connection
            .execute("DELETE FROM xmp_operations WHERE item=?1", [item])
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn schedule_xmp(
        &self,
        old: Option<Export>,
        new: Option<(Export, String)>,
        summary: &mut XmpSummary,
    ) -> Result<()> {
        let item = new
            .as_ref()
            .map(|(r, _)| r.item.clone())
            .or_else(|| old.as_ref().map(|r| r.item.clone()))
            .ok_or("Missing XMP owner.")?;
        let stage = if let Some((record, text)) = &new {
            if text.len() as u64 > MAX_XMP {
                return Err("Merged XMP would exceed the safe 8 MB size limit.".into());
            }
            let relative = format!(".singulo/staging/xmp-{}", Uuid::new_v4());
            let path = paths::inside(&self.root, Path::new(&relative))?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            self.flush_xmp_folder(&relative)?;
            self.verify_xmp(record, &relative)?;
            Some(relative)
        } else {
            None
        };
        let recovery = format!(".singulo/recovery/xmp-{}", Uuid::new_v4());
        if old.is_some() {
            // Reserve a new private directory, not just an apparently free file
            // name. Relocation must never overwrite another recovery file.
            fs::create_dir(paths::inside(&self.root, Path::new(&recovery))?)
                .map_err(|e| format!("Cannot reserve XMP recovery space: {e}"))?;
            self.flush_xmp_folder(&recovery)?;
        }
        let mut pending = Pending {
            old,
            new: new.map(|(record, _)| record),
            stage,
            parked: format!("{recovery}/previous.xmp"),
            committed: false,
            discarded: None,
        };
        self.save_xmp_pending(&item, &pending)?;
        self.finish_xmp(&item, &mut pending, summary).map_err(|error| {
            format!("XMP operation needs recovery (intent retained, recovery path {}, stage {:?}): {error}",
                pending.parked, pending.stage)
        })
    }

    fn sync_xmp_item(&self, id: &str, summary: &mut XmpSummary) -> Result<bool> {
        self.check()?;
        let pending: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM xmp_operations WHERE item=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if pending {
            return Err(
                "A previous sidecar operation still needs recovery; no new export was attempted."
                    .into(),
            );
        }
        let item = self.item(id)?;
        let old = self.xmp_record(id)?;
        if item.media.kind == "sidecar" {
            return Ok(true);
        }
        if item.path.starts_with(".singulo/staging/")
            || matches!(item.state.as_str(), "purge-pending" | "trash-pending")
        {
            return Ok(false);
        }
        if matches!(item.state.as_str(), "undo" | "purged") {
            if item.state == "purged" {
                self.connection
                    .execute("DELETE FROM xmp_baselines WHERE item=?1", [id])
                    .map_err(|e| e.to_string())?;
            }
            if let Some(old) = old {
                if item.state == "undo" {
                    self.connection.execute(
                        "INSERT INTO xmp_baselines VALUES(?1,?2) ON CONFLICT(item) DO UPDATE SET base=excluded.base",
                        params![id, old.base],
                    ).map_err(|e| e.to_string())?;
                }
                self.schedule_xmp(Some(old), None, summary)?;
            }
            return Ok(true);
        }
        if item.state != "active" {
            return Err("The original is in an unsupported state; its sidecar was kept.".into());
        }
        let original = paths::inside(&self.root, Path::new(&item.path))?;
        // This writes metadata only, not a verified copy of the media. Import,
        // relocation and source cleanup retain their independent hash checks.
        if !fs::symlink_metadata(&original)
            .map_err(|e| format!("Archived original is unavailable; no sidecar was exported: {e}"))?
            .is_file()
        {
            return Err("Archived original is not a regular file; no sidecar was exported.".into());
        }
        if let Some(old) = &old {
            self.verify_xmp(old, &old.path)?;
        }
        let preferred = format!("{}.xmp", item.path);
        let occupied = self.xmp_collision(&preferred)?;
        if old.as_ref().is_none_or(|old| old.path != preferred) && occupied {
            return Err(format!(
                "Preferred Immich sidecar name is occupied; export skipped: {preferred}"
            ));
        }
        let mut fallbacks = Vec::new();
        let fallback = PathBuf::from(&item.path)
            .with_extension("xmp")
            .to_string_lossy()
            .replace('\\', "/");
        if fallback != preferred && self.xmp_collision(&fallback)? {
            // A differently cased name is a collision, not permission to read an
            // arbitrary other file on a case-sensitive filesystem.
            if !self.xmp_exists(&fallback)? {
                return Err(format!(
                    "Fallback XMP has a case-colliding name: {fallback}"
                ));
            }
            fallbacks.push(fallback);
        }
        for companion in self.companions(id)? {
            let companion = self.item(&companion)?;
            if companion.media.kind == "sidecar"
                && companion.state != "purged"
                && !fallbacks.contains(&companion.path)
            {
                let path = paths::inside(&self.root, Path::new(&companion.path))?;
                if !self.verified_file(&path, &companion.media.hash)? {
                    return Err(format!(
                        "Imported sidecar is missing or changed: {}",
                        companion.path
                    ));
                }
                fallbacks.push(companion.path);
            }
        }
        if fallbacks.len() > 1 {
            return Err(
                "Multiple different fallback sidecars need a manual merge; none was overwritten."
                    .into(),
            );
        }
        let base = if let Some(fallback) = fallbacks.first() {
            self.read_xmp(fallback)?
        } else {
            match &old {
                Some(old) => old.base.clone(),
                None => self
                    .connection
                    .query_row("SELECT base FROM xmp_baselines WHERE item=?1", [id], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(|e| e.to_string())?
                    .unwrap_or_else(|| EMPTY.into()),
            }
        };
        let text = render(&base, &self.id, &item)?;
        let new = Export {
            item: id.into(),
            path: preferred,
            hash: digest(text.as_bytes()),
            base,
        };
        if old
            .as_ref()
            .is_some_and(|old| old.path == new.path && old.hash == new.hash)
        {
            summary.skipped += 1;
        } else {
            self.schedule_xmp(old, Some((new, text)), summary)?;
        }
        if item.media.date_source == "manual"
            && item
                .media
                .date
                .as_ref()
                .is_some_and(|date| date.len() == 10)
        {
            // Immich firstDateTime() uses ExifDateTime.fromEXIF(), which rejects
            // values without both date and time:
            // https://github.com/immich-app/immich/blob/6b9c85c8ee194a6077940638c9de01f80f5e810b/server/src/services/metadata.service.ts#L59-L81
            // https://github.com/photostructure/exiftool-vendored.js/blob/6a8f371d62692366721ff07538e109d34e9da04f/src/ExifDateTime.ts#L60-L95
            summary.errors.push(format!(
                "{}: The day-only correction was preserved in XMP, but Immich requires a date with a time and will not apply it as a timestamp. No time or timezone was invented.",
                item.path
            ));
        }
        Ok(true)
    }

    fn xmp_ids(&self, table: &str, after: &str) -> Result<Vec<String>> {
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT item FROM {table} WHERE item>?1 ORDER BY item LIMIT 128"
            ))
            .map_err(|e| e.to_string())?;
        query
            .query_map([after], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn export_xmp(&self) -> Result<XmpSummary> {
        self.check()?;
        self.connection
            .execute_batch(
                "BEGIN IMMEDIATE;
             INSERT OR REPLACE INTO config VALUES('xmp-enabled','true');
             INSERT OR IGNORE INTO xmp_dirty SELECT id FROM items
                 WHERE state='active' AND json_extract(data,'$.media.kind')!='sidecar';
             INSERT OR IGNORE INTO xmp_dirty SELECT item FROM exported_xmp;
             COMMIT;",
            )
            .map_err(|e| e.to_string())?;
        self.xmp_work
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.xmp_exporting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut summary = self.sync_exported_xmp()?;
        if let Err(error) = self.backup() {
            summary.errors.push(format!(
                "XMP export finished, but its recovery backup failed: {error}"
            ));
        }
        Ok(summary)
    }

    pub fn remove_exported_xmp(&self) -> Result<XmpSummary> {
        self.check()?;
        self.connection
            .execute_batch(
                "BEGIN IMMEDIATE;
             INSERT OR REPLACE INTO config VALUES('xmp-enabled','removing');
             DELETE FROM xmp_dirty;
             DELETE FROM xmp_baselines;
             COMMIT;",
            )
            .map_err(|e| e.to_string())?;
        self.xmp_work
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.xmp_exporting
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let mut summary = self.sync_exported_xmp()?;
        if let Err(error) = self.backup() {
            summary.errors.push(format!(
                "XMP removal finished, but its recovery backup failed: {error}"
            ));
        }
        Ok(summary)
    }

    pub fn sync_exported_xmp(&self) -> Result<XmpSummary> {
        if !self.xmp_work.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(XmpSummary::default());
        }
        self.check()?;
        if !self.connection.is_autocommit() {
            return Err("XMP export cannot run inside a catalog transaction.".into());
        }
        self.xmp_names.lock().map_err(|e| e.to_string())?.clear();
        let mut summary = XmpSummary::default();
        if let Err(error) = self.sync_xmp_inner(&mut summary) {
            summary.errors.push(format!(
                "XMP processing stopped after partial work; the counts include completed file changes. Unfinished operations remain recoverable: {error}"
            ));
        }
        Ok(summary)
    }

    fn sync_xmp_inner(&self, summary: &mut XmpSummary) -> Result<()> {
        let mode = self.xmp_mode()?;
        let current_policy: bool = if mode == "true" {
            self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM config WHERE key='xmp-policy' AND value='manual-active-keywords-v3')",
                [], |r| r.get(0)
            ).map_err(|e| e.to_string())?
        } else {
            true
        };
        if !current_policy {
            // Reconcile older date/activity/tag-precedence policies, including
            // inactive items no longer present in the dirty queue.
            self.connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                 INSERT INTO xmp_dirty SELECT item FROM exported_xmp WHERE true
                     ON CONFLICT(item) DO NOTHING;
                 INSERT INTO config VALUES('xmp-policy','manual-active-keywords-v3')
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value;
                 COMMIT;",
                )
                .map_err(|e| e.to_string())?;
        }
        let mut after = String::new();
        let mut blocked = HashSet::new();
        loop {
            let ids = self.xmp_ids("xmp_operations", &after)?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                after = id.clone();
                let completed = summary.created + summary.updated + summary.removed;
                let result = (|| {
                    let data: String = self
                        .connection
                        .query_row(
                            "SELECT data FROM xmp_operations WHERE item=?1",
                            [&id],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    self.finish_xmp(&id, &mut parse(&data)?, summary)
                })();
                if let Err(error) = result {
                    if completed == summary.created + summary.updated + summary.removed {
                        summary.skipped += 1;
                    }
                    summary.errors.push(self.xmp_attention(&id, format_args!(
                        "Interrupted XMP operation needs recovery; retained staging/recovery bytes were not discarded: {error}"
                    )));
                    blocked.insert(id);
                }
            }
        }
        let table = if mode == "true" {
            "xmp_dirty"
        } else if mode == "removing" {
            "exported_xmp"
        } else {
            let pending: bool = self
                .connection
                .query_row("SELECT EXISTS(SELECT 1 FROM xmp_operations)", [], |r| {
                    r.get(0)
                })
                .map_err(|e| e.to_string())?;
            self.xmp_work
                .store(pending, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        };
        after.clear();
        loop {
            let ids = self.xmp_ids(table, &after)?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                after = id.clone();
                if blocked.contains(&id) {
                    continue;
                }
                let completed = summary.created + summary.updated + summary.removed;
                let result = if mode == "true" {
                    self.sync_xmp_item(&id, summary)
                } else {
                    let pending: bool = self
                        .connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM xmp_operations WHERE item=?1)",
                            [&id],
                            |r| r.get(0),
                        )
                        .map_err(|e| self.xmp_attention(&id, e))?;
                    if pending {
                        Err(
                            "Sidecar removal is waiting for recovery of its previous operation."
                                .into(),
                        )
                    } else {
                        self.xmp_record(&id)
                            .map_err(|e| self.xmp_attention(&id, e))?
                            .ok_or("Missing XMP ownership record.".into())
                            .and_then(|old| {
                                self.verify_xmp(&old, &old.path)?;
                                self.schedule_xmp(Some(old), None, summary)?;
                                Ok(true)
                            })
                    }
                };
                match result {
                    Ok(true) if mode == "true" => {
                        self.connection
                            .execute("DELETE FROM xmp_dirty WHERE item=?1", [&id])
                            .map_err(|e| self.xmp_attention(&id, e))?;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        if completed == summary.created + summary.updated + summary.removed {
                            summary.skipped += 1;
                        }
                        summary.errors.push(self.xmp_attention(&id, error));
                    }
                }
            }
        }
        if mode == "removing" {
            self.connection
                .execute(
                    "UPDATE config SET value='false' WHERE key='xmp-enabled'
                 AND NOT EXISTS(SELECT 1 FROM exported_xmp)
                 AND NOT EXISTS(SELECT 1 FROM xmp_operations)",
                    [],
                )
                .map_err(|e| e.to_string())?;
            if self.xmp_mode()? == "false" {
                self.xmp_work
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        Ok(())
    }
}
