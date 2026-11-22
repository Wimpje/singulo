use crate::{archive::Archive, media, model::*, paths};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use std::{fs, path::Path};

fn sample(hash: String) -> Media {
    Media {
        kind: "photo".into(),
        date: Some("2023-07-18T14:32:10".into()),
        date_source: "capture metadata".into(),
        warnings: vec![],
        width: 100,
        height: 100,
        duration_seconds: None,
        hash,
        bytes: 4,
        signature: None,
        metadata: serde_json::json!({}),
    }
}

#[test]
fn deferred_thumbnail_folders_are_conservatively_classified() {
    let temp = tempfile::Builder::new().prefix(".filter-test-").tempdir_in(".").unwrap();
    let root = temp.path();
    let mut files = Vec::new();
    for (folder, name, size) in [
        ("small", "one.png", (300, 300)),
        ("small", "two.png", (1, 200)),
        ("mixed", "small.png", (200, 200)),
        ("mixed", "large.png", (301, 300)),
        ("unknown", "small.png", (50, 50)),
        ("Cache/nested", "large.png", (500, 500)),
        (".thumbnails", "large.png", (500, 500)),
        ("node_modules/package/images", "large.png", (500, 500)),
        ("steamapps/common/Game/assets/textures", "large.png", (500, 500)),
        ("Viewer.app/Contents/Resources", "large.png", (500, 500)),
        ("Game_Data/StreamingAssets", "large.png", (500, 500)),
        ("family/assets", "large.png", (500, 500)),
        ("family/icons", "large.png", (500, 500)),
        ("steamapps/common/Game/Screenshots", "large.png", (500, 500)),
    ] {
        let directory = root.join(folder);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        image::RgbImage::new(size.0, size.1).save(&path).unwrap();
        files.push((root.to_path_buf(), path));
    }
    let raw = root.join("unknown").join("photo.heic");
    fs::write(&raw, b"unknown dimensions").unwrap();
    files.push((root.to_path_buf(), raw));
    #[cfg(unix)]
    let unusual = {
        let folder = root.join(r"family\node_modules\wedding");
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("large.png");
        image::RgbImage::new(500, 500).save(&path).unwrap();
        files.push((root.to_path_buf(), path));
        folder
    };
    let complete = [root.to_path_buf()].into_iter().collect();
    let cancel = crate::cancel::Cancellation::default();
    let found = crate::import::classify_deferred_folders(&files, &complete, &cancel, |_| Ok(())).unwrap();
    assert_eq!(found.len(), 7);
    assert_eq!(found[&root.join("small")].count, 2);
    assert_eq!(found[&root.join("small")].reason, "small");
    assert_eq!(found[&root.join("Cache/nested")].reason, "named");
    for folder in [
        ".thumbnails", "node_modules/package/images", "steamapps/common/Game/assets/textures",
        "Viewer.app/Contents/Resources", "Game_Data/StreamingAssets",
    ] {
        assert_eq!(found[&root.join(folder)].reason, "named");
    }
    for folder in ["family/assets", "family/icons", "steamapps/common/Game/Screenshots"] {
        assert!(!found.contains_key(&root.join(folder)));
    }
    #[cfg(unix)]
    assert!(!found.contains_key(&unusual));
    let incomplete = crate::import::classify_deferred_folders(&files, &Default::default(), &cancel, |_| Ok(())).unwrap();
    assert_eq!(incomplete.len(), 6);
    assert!(incomplete.contains_key(&root.join("Cache/nested")));
    let nested = root.join("Cache/nested");
    let files = vec![(nested.clone(), nested.join("large.png"))];
    let found = crate::import::classify_deferred_folders(&files, &[nested.clone()].into_iter().collect(), &cancel, |_| Ok(())).unwrap();
    assert_eq!(found[&nested].reason, "named");
    cancel.request();
    assert!(crate::import::classify_deferred_folders(&files, &complete, &cancel, |_| Ok(())).is_err());
}

#[test]
fn deferred_folders_persist_and_are_scoped_to_the_computer() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    let folder = DeferredFolder {
        root: "/source".into(),
        path: "/source/thumbnails".into(),
        count: 4,
        reason: "named".into(),
    };
    archive.save_deferred_folders("one", std::slice::from_ref(&folder)).unwrap();
    archive.save_deferred_folders("two", std::slice::from_ref(&folder)).unwrap();
    drop(archive);
    let mut archive = Archive::open(&root).unwrap();
    assert_eq!(archive.deferred_folders("one").unwrap()[0].count, 4);
    archive.remove_deferred_folders("one", &[folder.path]).unwrap();
    assert!(archive.deferred_folders("one").unwrap().is_empty());
    assert_eq!(archive.deferred_folders("two").unwrap().len(), 1);
}

fn catalog_cleanup_item(archive: &mut Archive, source: &Path, id: &str, kind: &str, width: u32, height: u32) -> Occurrence {
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(source, id.as_bytes()).unwrap();
    let hash = media::hash(source).unwrap();
    let mut media = sample(hash.clone());
    media.kind = kind.into();
    media.width = width;
    media.height = height;
    media.bytes = id.len() as u64;
    let item = Item {
        id: id.into(),
        path: format!("catalog/{id}.jpg"),
        original_name: source.file_name().unwrap().to_string_lossy().into(),
        state: "active".into(),
        keeper: None,
        media,
        settings: Settings::default(),
        albums: vec![],
    };
    let occurrence = Occurrence {
        id: format!("occ-{id}"),
        item: id.into(),
        source: source.to_string_lossy().into(),
        root: source.parent().unwrap().to_string_lossy().into(),
        album: "source".into(),
        relative: source.file_name().unwrap().to_string_lossy().into(),
        hash,
        identity: media::identity(source).unwrap(),
        status: "pending".into(),
        bytes: id.len() as u64,
        computer: Some(Computer { id: "computer".into(), name: "Computer".into() }),
    };
    archive.place(source, item, Some(occurrence.clone()), None).unwrap();
    occurrence
}

#[test]
fn review_groups_hide_only_suspicious_origins_before_grouping() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    let source = temp.path().join("sources");
    for id in ["a", "b", "c", "d"] {
        catalog_cleanup_item(&mut archive, &source.join(format!("family/{id}.jpg")), id, "photo", 600, 600);
    }
    let mut hidden = catalog_cleanup_item(&mut archive, &source.join("cache/h.jpg"), "h", "photo", 600, 600);
    catalog_cleanup_item(&mut archive, &source.join("cache/h.dng"), "h-raw", "raw", 600, 600);
    catalog_cleanup_item(&mut archive, &source.join("small/s.jpg"), "s", "photo", 300, 300);
    archive.add_review("a", "b", "match").unwrap();
    archive.add_review("c", "d", "match").unwrap();
    archive.add_review("a", "h", "match").unwrap();
    archive.add_review("h", "c", "match").unwrap();
    archive.add_review("a", "s", "small match").unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    assert_eq!(page["total"], 2);
    let all = serde_json::to_value(archive.review_groups(true, 0).unwrap()).unwrap();
    assert_eq!(all["total"], 1);
    assert_eq!(all["groups"][0]["count"], 6);

    let extra = source.join("family/h.jpg");
    fs::copy(&hidden.source, &extra).unwrap();
    hidden.id = "other-h".into();
    hidden.source = extra.to_string_lossy().into();
    hidden.identity = media::identity(&extra).unwrap();
    archive.record_occurrence(&hidden).unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["groups"][0]["count"], 5);
}

#[test]
fn review_groups_do_not_move_companions_apart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    catalog_cleanup_item(&mut archive, &temp.path().join("family/a.jpg"), "a", "photo", 600, 600);
    catalog_cleanup_item(&mut archive, &temp.path().join("family/a.dng"), "raw", "raw", 600, 600);
    catalog_cleanup_item(&mut archive, &temp.path().join("family/b.jpg"), "b", "photo", 600, 600);
    archive.add_review("a", "b", "match").unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    let group = &page["groups"][0];
    assert!(archive.resolve_review_group(false, group["id"].as_str().unwrap(), group["token"].as_str().unwrap(), &["a".into()]).is_err());
    assert_eq!(archive.item("a").unwrap().state, "active");
    assert_eq!(archive.item("raw").unwrap().state, "active");
}

#[test]
fn review_group_details_page_pictures_and_keep_direct_match_numbers() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    for index in 0..10 {
        let id = format!("p{index}");
        catalog_cleanup_item(&mut archive, &temp.path().join(format!("family/{id}.jpg")), &id, "photo", 600, 600);
        if index > 0 {
            archive.add_review(&format!("p{}", index - 1), &id, "match").unwrap();
        }
    }
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    let group = &page["groups"][0];
    let id = group["id"].as_str().unwrap();
    let token = group["token"].as_str().unwrap();
    let first = serde_json::to_value(archive.review_group_detail(false, id, token, 0).unwrap()).unwrap();
    let second = serde_json::to_value(archive.review_group_detail(false, id, token, 8).unwrap()).unwrap();
    assert_eq!(first["count"], 10);
    assert_eq!(first["items"].as_array().unwrap().len(), 8);
    assert_eq!(second["items"].as_array().unwrap().len(), 2);
    assert_eq!(second["links"][0]["numbers"], serde_json::json!([8, 10]));
}

#[test]
fn review_groups_require_direct_matches_and_can_be_undone() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    for id in ["a", "b", "c"] {
        catalog_cleanup_item(&mut archive, &temp.path().join(format!("family/{id}.jpg")), id, "photo", 600, 600);
    }
    archive.add_review("a", "b", "first").unwrap();
    archive.add_review("b", "c", "second").unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    let group = &page["groups"][0];
    let id = group["id"].as_str().unwrap();
    let token = group["token"].as_str().unwrap();
    assert!(archive.resolve_review_group(false, id, token, &["b".into(), "c".into()]).is_err());
    assert_eq!(archive.item("b").unwrap().state, "active");
    let outcome = serde_json::to_value(archive.resolve_review_group(false, id, token, &["a".into(), "c".into()]).unwrap()).unwrap();
    assert_eq!(outcome["moved"], 2);
    assert_eq!(archive.item("a").unwrap().state, "undo");
    assert_eq!(archive.item("c").unwrap().state, "undo");
    assert_eq!(serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap()["total"], 0);
    assert!(archive.resolve_review_group(false, id, token, &[]).is_err());
    archive.undo_review_group(outcome["token"].as_str().unwrap()).unwrap();
    assert_eq!(archive.item("a").unwrap().state, "active");
    assert_eq!(archive.item("c").unwrap().state, "active");
    assert_eq!(serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap()["total"], 1);
    assert_eq!(archive.reviews().unwrap().len(), 2);
}

#[test]
fn review_group_partial_failure_keeps_original_links_and_allows_undo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    for id in ["a", "b", "c"] {
        catalog_cleanup_item(&mut archive, &temp.path().join(format!("family/{id}.jpg")), id, "photo", 600, 600);
    }
    archive.add_review("a", "b", "first").unwrap();
    archive.add_review("b", "c", "second").unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    let group = &page["groups"][0];
    fs::write(root.join(archive.item("c").unwrap().path), b"changed").unwrap();
    let result = serde_json::to_value(archive.resolve_review_group(
        false, group["id"].as_str().unwrap(), group["token"].as_str().unwrap(),
        &["a".into(), "c".into()],
    ).unwrap()).unwrap();
    assert_eq!(result["moved"], 1);
    assert!(!result["errors"].as_array().unwrap().is_empty());
    assert_eq!(archive.item("a").unwrap().state, "undo");
    assert_eq!(archive.item("c").unwrap().state, "active");
    archive.undo_review_group(result["token"].as_str().unwrap()).unwrap();
    assert_eq!(archive.item("a").unwrap().state, "active");
    assert_eq!(archive.reviews().unwrap().len(), 2);
}

#[test]
fn review_group_does_not_restore_over_a_pending_operation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    for id in ["a", "b"] {
        catalog_cleanup_item(&mut archive, &temp.path().join(format!("family/{id}.jpg")), id, "photo", 600, 600);
    }
    archive.add_review("a", "b", "match").unwrap();
    let page = serde_json::to_value(archive.review_groups(false, 0).unwrap()).unwrap();
    let group = &page["groups"][0];
    let result = serde_json::to_value(archive.resolve_review_group(
        false, group["id"].as_str().unwrap(), group["token"].as_str().unwrap(), &["a".into()],
    ).unwrap()).unwrap();
    let connection = rusqlite::Connection::open(root.join(".singulo/catalog.sqlite")).unwrap();
    connection.execute(
        "INSERT INTO operations VALUES('pending-test', ?1)",
        [serde_json::json!({"item": {"id": "a"}}).to_string()],
    ).unwrap();
    let token = result["token"].as_str().unwrap();
    let blocked = serde_json::to_value(archive.undo_review_group(token).unwrap()).unwrap();
    assert_eq!(blocked["retryable"], true);
    assert!(!blocked["errors"].as_array().unwrap().is_empty());
    assert_eq!(archive.item("a").unwrap().state, "undo");
    connection.execute("DELETE FROM operations WHERE id='pending-test'", []).unwrap();
    archive.undo_review_group(token).unwrap();
    assert_eq!(archive.item("a").unwrap().state, "active");
}

#[test]
fn catalog_cleanup_checks_history_and_preserves_copies_on_failure() {
    let temp = tempfile::Builder::new().prefix(".filter-test-").tempdir_in(".").unwrap();
    let archive_root = temp.path().join("archive");
    fs::create_dir(&archive_root).unwrap();
    let mut archive = Archive::open(&archive_root).unwrap();
    let sources = temp.path().join("sources");
    catalog_cleanup_item(&mut archive, &sources.join("cache/large.jpg"), "cache", "photo", 600, 600);
    catalog_cleanup_item(&mut archive, &sources.join("small/a.jpg"), "small-a", "photo", 300, 300);
    catalog_cleanup_item(&mut archive, &sources.join("small/b.jpg"), "small-b", "photo", 1, 200);
    catalog_cleanup_item(&mut archive, &sources.join("mixed/a.jpg"), "mixed-a", "photo", 100, 100);
    catalog_cleanup_item(&mut archive, &sources.join("mixed/b.jpg"), "mixed-b", "photo", 301, 300);
    catalog_cleanup_item(&mut archive, &sources.join("unknown/a.jpg"), "unknown", "photo", 0, 0);
    let shared = catalog_cleanup_item(&mut archive, &sources.join("cache/shared.jpg"), "shared", "photo", 600, 600);
    let other_source = sources.join("family/shared.jpg");
    fs::create_dir_all(other_source.parent().unwrap()).unwrap();
    fs::copy(&shared.source, &other_source).unwrap();
    let mut other = shared.clone();
    other.id = "other-occurrence".into();
    other.source = other_source.to_string_lossy().into();
    other.root = other_source.parent().unwrap().to_string_lossy().into();
    other.identity = media::identity(&other_source).unwrap();
    archive.record_occurrence(&other).unwrap();
    let mut legacy = catalog_cleanup_item(&mut archive, &sources.join("cache/legacy.jpg"), "legacy", "photo", 600, 600);
    legacy.id = "legacy-occurrence".into();
    legacy.source = "unlocated.jpg".into();
    archive.record_occurrence(&legacy).unwrap();
    catalog_cleanup_item(&mut archive, &sources.join("cache/buddy.jpg"), "buddy", "photo", 400, 400);
    catalog_cleanup_item(&mut archive, &sources.join("cache/buddy.dng"), "buddy-raw", "raw", 400, 400);

    let preview = archive.catalog_cleanup_preview().unwrap();
    let cache = preview.iter().find(|folder| folder.path == sources.join("cache").to_string_lossy()).unwrap().clone();
    let small = preview.iter().find(|folder| folder.path == sources.join("small").to_string_lossy()).unwrap().clone();
    assert_eq!((preview.len(), cache.count, small.count), (2, 1, 2));
    catalog_cleanup_item(&mut archive, &sources.join("small/c.jpg"), "small-c", "photo", 80, 80);
    assert!(archive.catalog_cleanup_with(&[small], |_| panic!("stale preview must not trash files")).is_err());
    assert_eq!(archive.item("small-a").unwrap().state, "active");

    let trash = temp.path().join("trash");
    fs::create_dir(&trash).unwrap();
    let result = archive.catalog_cleanup_with(&[cache], |path| {
        fs::rename(path, trash.join("cache.jpg")).map_err(|error| error.to_string())
    }).unwrap();
    assert_eq!(result.trashed, 1);
    assert!(result.errors.is_empty());
    assert_eq!(archive.item("cache").unwrap().state, "purged");
    assert_eq!(fs::read(trash.join("cache.jpg")).unwrap(), b"cache");
    assert_eq!(fs::read(sources.join("cache/large.jpg")).unwrap(), b"cache");
    assert_eq!(archive.item("shared").unwrap().state, "active");
    assert_eq!(archive.item("legacy").unwrap().state, "active");
    assert_eq!(archive.item("buddy").unwrap().state, "active");

    let small = archive.catalog_cleanup_preview().unwrap().into_iter()
        .find(|folder| folder.path == sources.join("small").to_string_lossy()).unwrap();
    let active_path = archive.root.join(archive.item("small-a").unwrap().path);
    fs::write(&active_path, b"changed").unwrap();
    let result = archive.catalog_cleanup_with(std::slice::from_ref(&small), |_| panic!("changed archive files must not be trashed")).unwrap();
    assert_eq!(result.trashed, 0);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(archive.item("small-a").unwrap().state, "active");
    fs::write(&active_path, b"small-a").unwrap();
    let result = archive.catalog_cleanup_with(&[small], |_| Err("Trash unavailable".into())).unwrap();
    assert_eq!(result.trashed, 0);
    assert_eq!(result.errors.len(), 1);
    let retained = archive.item("small-a").unwrap();
    assert_eq!(retained.state, "undo");
    assert_eq!(fs::read(archive.root.join(retained.path)).unwrap(), b"small-a");
}

#[test]
fn catalog_cleanup_requires_all_matching_origins_for_shared_items() {
    let temp = tempfile::Builder::new().prefix(".filter-test-").tempdir_in(".").unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    let first = temp.path().join("cache/photo.jpg");
    let mut occurrence = catalog_cleanup_item(&mut archive, &first, "shared", "photo", 600, 600);
    let second = temp.path().join("temp/photo.jpg");
    fs::create_dir_all(second.parent().unwrap()).unwrap();
    fs::copy(&first, &second).unwrap();
    occurrence.id = "second-occurrence".into();
    occurrence.source = second.to_string_lossy().into();
    occurrence.root = second.parent().unwrap().to_string_lossy().into();
    occurrence.identity = media::identity(&second).unwrap();
    archive.record_occurrence(&occurrence).unwrap();

    let preview = archive.catalog_cleanup_preview().unwrap();
    assert_eq!(preview.len(), 2);
    let partial = archive.catalog_cleanup_with(&preview[..1], |_| panic!("incomplete selection must not trash files")).unwrap();
    assert_eq!((partial.trashed, partial.skipped_shared), (0, 1));
    assert_eq!(archive.item("shared").unwrap().state, "active");
    let trash = temp.path().join("trash.jpg");
    let complete = archive.catalog_cleanup_with(&preview, |path| {
        fs::rename(path, &trash).map_err(|error| error.to_string())
    }).unwrap();
    assert_eq!((complete.trashed, complete.skipped_shared), (1, 0));
    assert!(complete.errors.is_empty());
    assert_eq!(fs::read(&trash).unwrap(), b"shared");
    assert!(first.exists() && second.exists());
}

#[test]
fn catalog_cleanup_previews_app_assets_but_not_generic_photo_folders() {
    let temp = tempfile::Builder::new().prefix(".filter-test-").tempdir_in(".").unwrap();
    let root = temp.path().join("archive");
    fs::create_dir(&root).unwrap();
    let mut archive = Archive::open(&root).unwrap();
    for (folder, id) in [
        ("project/node_modules/package/icons", "dependency"),
        ("steamapps/common/Game/assets", "game-asset"),
        ("Viewer.app/Contents/Resources", "app-resource"),
        ("family/assets", "family-assets"),
        ("family/icons", "family-icons"),
        ("steamapps/common/Game/Screenshots", "screenshot"),
    ] {
        catalog_cleanup_item(&mut archive, &temp.path().join(folder).join("photo.jpg"), id, "photo", 600, 600);
    }
    #[cfg(unix)]
    catalog_cleanup_item(&mut archive, &temp.path().join(r"family\node_modules\wedding").join("photo.jpg"), "backslash-name", "photo", 600, 600);
    let folders = archive.catalog_cleanup_preview().unwrap();
    assert_eq!(folders.len(), 3);
    for folder in [
        "project/node_modules/package/icons", "steamapps/common/Game/assets", "Viewer.app/Contents/Resources",
    ] {
        assert!(folders.iter().any(|entry| entry.path == temp.path().join(folder).to_string_lossy()));
    }
    for id in ["dependency", "game-asset", "app-resource", "family-assets", "family-icons", "screenshot"] {
        assert_eq!(archive.item(id).unwrap().state, "active");
    }
}

fn xmp_fixture() -> (tempfile::TempDir, Archive) {
    let directory = tempfile::Builder::new()
        .prefix(".xmp-test-")
        .tempdir_in(".")
        .unwrap();
    let root = directory.path().join("archive");
    fs::create_dir(&root).unwrap();
    let archive = Archive::open(&root).unwrap();
    (directory, archive)
}

fn xmp_original(directory: &Path, archive: &mut Archive, id: &str) -> String {
    let source = directory.join(format!("{id}.jpg"));
    fs::write(&source, id.as_bytes()).unwrap();
    let mut media = sample(media::hash(&source).unwrap());
    media.bytes = id.len() as u64;
    let path = format!("2023/07/18/{id}.jpg");
    archive
        .place(
            &source,
            Item {
                id: id.into(),
                path: path.clone(),
                original_name: format!("{id}.jpg"),
                state: "active".into(),
                keeper: None,
                media,
                settings: Settings::default(),
                albums: vec!["Family & friends".into()],
            },
            None,
            None,
        )
        .unwrap();
    path
}

#[test]
fn xmp_merges_fallback_without_changing_originals_and_removes_only_owned_exports() {
    let (directory, mut archive) = xmp_fixture();
    let path = xmp_original(directory.path(), &mut archive, "photo");
    let original = archive.root.join(&path);
    let before = fs::read(&original).unwrap();
    let fallback = original.with_extension("xmp");
    let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:custom="urn:family" xmp:CreateDate="2001-01-01" custom:Rating="5">
<custom:People><rdf:Seq><rdf:li>Aunt &amp; uncle</rdf:li></rdf:Seq></custom:People>
<dc:subject><rdf:Bag><rdf:li>Existing</rdf:li></rdf:Bag></dc:subject></rdf:Description>
<rdf:Description rdf:about="another.jpg" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:CreateDate>1999-01-01</xmp:CreateDate></rdf:Description>
</rdf:RDF></x:xmpmeta>"#;
    let imported_source = directory.path().join("photo.xmp");
    fs::write(&imported_source, xml).unwrap();
    let mut imported = archive.item("photo").unwrap();
    imported.id = "imported-xmp".into();
    imported.path = fallback
        .strip_prefix(&archive.root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    imported.original_name = "photo.xmp".into();
    imported.media.kind = "sidecar".into();
    imported.media.hash = media::hash(&imported_source).unwrap();
    imported.media.bytes = xml.len() as u64;
    archive
        .place(&imported_source, imported, None, None)
        .unwrap();
    let summary = archive.xmp_worker().unwrap().export_xmp().unwrap();
    assert_eq!(
        (summary.created, summary.updated, summary.skipped),
        (1, 0, 0)
    );
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let exported = archive.root.join(format!("{path}.xmp"));
    let text = fs::read_to_string(&exported).unwrap();
    assert!(text.contains("custom:Rating=\"5\""));
    assert!(text.contains(
        "<custom:People><rdf:Seq><rdf:li>Aunt &amp; uncle</rdf:li></rdf:Seq></custom:People>"
    ));
    assert!(text.contains("Existing"));
    assert!(text.contains("Family &amp; friends"));
    assert!(!text.contains("2023-07-18T14:32:10"));
    assert!(text.contains(">1999-01-01<"));
    assert!(text.contains("2001-01-01"));
    assert_eq!(fs::read_to_string(&fallback).unwrap(), xml);
    assert_eq!(fs::read(&original).unwrap(), before);
    assert_eq!(archive.export_xmp().unwrap().created, 0);
    let summary = archive.xmp_worker().unwrap().remove_exported_xmp().unwrap();
    assert_eq!(summary.removed, 1);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert!(!exported.exists());
    assert_eq!(fs::read_to_string(&fallback).unwrap(), xml);
    assert_eq!(fs::read_to_string(imported_source).unwrap(), xml);
    assert_eq!(
        archive.item("imported-xmp").unwrap().media.hash,
        media::hash(&fallback).unwrap()
    );
    assert_eq!(fs::read(original).unwrap(), before);
    assert_eq!(
        fs::read(directory.path().join("photo.jpg")).unwrap(),
        before
    );
}

#[test]
fn xmp_reports_modified_and_colliding_files_without_hiding_partial_success() {
    let (directory, mut archive) = xmp_fixture();
    let changed = xmp_original(directory.path(), &mut archive, "changed");
    let safe = xmp_original(directory.path(), &mut archive, "safe");
    let occupied = xmp_original(directory.path(), &mut archive, "occupied");
    let collision = archive.root.join(format!("{occupied}.xmp"));
    fs::write(&collision, b"Imported or user-owned sidecar").unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!((summary.created, summary.skipped), (2, 1));
    assert_eq!(summary.errors.len(), 1);
    assert!(summary.errors[0].starts_with(&format!("{occupied}:")));
    let modified = archive.root.join(format!("{changed}.xmp"));
    let original_export = fs::read(&modified).unwrap();
    fs::write(
        &modified,
        [original_export.as_slice(), b"<!-- user's edit -->"].concat(),
    )
    .unwrap();
    let edited = fs::read(&modified).unwrap();
    let summary = archive.remove_exported_xmp().unwrap();
    assert_eq!((summary.removed, summary.skipped), (1, 1));
    assert_eq!(summary.errors.len(), 1);
    assert!(summary.errors[0].contains("Modified"));
    assert!(summary.errors[0].starts_with(&format!("{changed}:")));
    let marker = archive.root.join(".singulo/archive-id");
    let saved = marker.with_extension("saved");
    fs::rename(&marker, &saved).unwrap();
    assert!(archive.xmp_worker_if_needed().unwrap().is_none());
    fs::rename(saved, marker).unwrap();
    assert_eq!(fs::read(&modified).unwrap(), edited);
    assert!(!archive.root.join(format!("{safe}.xmp")).exists());
    assert_eq!(
        fs::read(&collision).unwrap(),
        b"Imported or user-owned sidecar"
    );
    fs::write(&modified, original_export).unwrap();
    assert_eq!(archive.remove_exported_xmp().unwrap().removed, 1);
    assert!(archive.recover().unwrap().is_empty());
    let disabled = xmp_original(directory.path(), &mut archive, "after-disable");
    assert_eq!(archive.sync_exported_xmp().unwrap().created, 0);
    assert!(!archive.root.join(format!("{disabled}.xmp")).exists());
}

#[test]
fn xmp_dates_only_override_fallbacks_for_manual_corrections_without_inventing_time() {
    let (directory, mut archive) = xmp_fixture();
    let xml = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/"
 xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmlns:exifEX="http://cipa.jp/exif/1.0/"
 exif:SubSecDateTimeOriginal="2001-01-01T01:02:03.456+05:30" xmp:SubSecCreateDate="2001-01-01T01:02:03.456+05:30"
 xmp:CreationDate="2001-01-01" xmp:MediaCreateDate="2001-01-01" xmp:ModifyDate="2005-05-05">
<exif:DateTimeOriginal>2001-01-01T01:02:03+05:30</exif:DateTimeOriginal>
<xmp:CreateDate>2001-01-01T01:02:03+05:30</xmp:CreateDate>
<xmp:SubSecMediaCreateDate>2001-01-01T01:02:03.456+05:30</xmp:SubSecMediaCreateDate>
<xmp:DateTimeCreated>2001-01-01T01:02:03+05:30</xmp:DateTimeCreated>
<photoshop:DateCreated>2001-01-01</photoshop:DateCreated>
<exif:SubSecTimeOriginal>456</exif:SubSecTimeOriginal><exifEX:OffsetTimeOriginal>+05:30</exifEX:OffsetTimeOriginal>
</rdf:Description></rdf:RDF>"#;
    let connection =
        rusqlite::Connection::open(archive.root.join(".singulo/catalog.sqlite")).unwrap();
    for (id, source, date) in [
        ("capture", "capture metadata", "2024-06-07T08:09:10"),
        (
            "estimated",
            "estimated: file modification",
            "2024-06-07T08:09:10",
        ),
        ("manual-time", "manual", "2024-06-07T08:09:10"),
        ("manual-day", "manual", "2024-06-07"),
    ] {
        let path = xmp_original(directory.path(), &mut archive, id);
        connection.execute(
            "UPDATE items SET data=json_set(data,'$.media.date',?1,'$.media.dateSource',?2) WHERE id=?3",
            rusqlite::params![date, source, id],
        ).unwrap();
        fs::write(archive.root.join(&path).with_extension("xmp"), xml).unwrap();
    }
    let summary = archive.export_xmp().unwrap();
    assert_eq!(summary.created, 4);
    assert_eq!(summary.errors.len(), 1, "{:?}", summary.errors);
    assert!(summary.errors[0].starts_with("2023/07/18/manual-day.jpg:"));
    assert!(summary.errors[0].contains("Immich requires a date with a time"));
    for id in ["capture", "estimated", "manual-time", "manual-day"] {
        let item = archive.item(id).unwrap();
        let text = fs::read_to_string(archive.root.join(format!("{}.xmp", item.path))).unwrap();
        let doc = roxmltree::Document::parse(&text).unwrap();
        if item.media.date_source == "manual" {
            let date = item.media.date.as_deref().unwrap();
            for (ns, name) in [
                ("http://ns.adobe.com/exif/1.0/", "DateTimeOriginal"),
                ("http://ns.adobe.com/photoshop/1.0/", "DateCreated"),
            ] {
                let values: Vec<_> = doc
                    .descendants()
                    .filter(|n| n.has_tag_name((ns, name)))
                    .map(|n| n.text())
                    .collect();
                assert_eq!(values, [Some(date)]);
            }
            for stale in [
                "2001-01-01",
                "SubSecDateTimeOriginal",
                "SubSecCreateDate",
                "SubSecMediaCreateDate",
                "SubSecTimeOriginal",
                "OffsetTimeOriginal",
                "+05:30",
            ] {
                assert!(!text.contains(stale), "{id}: {stale}");
            }
            if id == "manual-day" {
                assert!(!text.contains("T00:00"));
                assert!(!text.contains("2024-06-07Z"));
            }
        } else {
            assert!(
                text.contains(
                    xml.split("<rdf:Description")
                        .nth(1)
                        .unwrap()
                        .split("</rdf:Description>")
                        .next()
                        .unwrap()
                )
            );
            assert!(!text.contains("2024-06-07"));
        }
        assert!(text.contains("2005-05-05"));
        assert_eq!(
            fs::read_to_string(archive.root.join(&item.path).with_extension("xmp")).unwrap(),
            xml
        );
        assert_eq!(
            fs::read(directory.path().join(format!("{id}.jpg"))).unwrap(),
            id.as_bytes()
        );
    }
}

#[test]
fn xmp_disabled_hooks_do_not_open_workers_or_check_unrelated_archive_files() {
    let (directory, mut archive) = xmp_fixture();
    xmp_original(directory.path(), &mut archive, "photo");
    let marker = archive.root.join(".singulo/archive-id");
    let saved = marker.with_extension("saved");
    fs::rename(&marker, &saved).unwrap();
    archive.dismiss_review("photo", "other").unwrap();
    assert!(archive.xmp_worker_if_needed().unwrap().is_none());
    assert!(archive.sync_exported_xmp().unwrap().errors.is_empty());
    let state = AppState::default();
    *state.archive.lock().unwrap() = Some(archive);
    assert!(crate::import::sync_xmp(&state).unwrap().is_empty());
    fs::rename(saved, marker).unwrap();
}

#[test]
fn xmp_merges_digikam_tags_before_folder_labels_can_be_shadowed() {
    let digikam = "http://www.digikam.org/ns/1.0/";
    for (attribute, body, extra, supported) in [
        (
            "",
            r#"<dk:TagsList><rdf:Bag><rdf:li>Old &amp; treasured</rdf:li><rdf:li>Family &amp; friends</rdf:li></rdf:Bag></dk:TagsList>"#,
            "",
            true,
        ),
        (r#"dk:TagsList="Old""#, "", "", false),
        (
            "",
            "<dk:TagsList><rdf:Seq><rdf:li>Old</rdf:li></rdf:Seq></dk:TagsList>",
            "",
            false,
        ),
        (
            "",
            r#"<dk:TagsList><rdf:Bag><rdf:li rdf:parseType="Resource"><dk:Name>Old</dk:Name></rdf:li></rdf:Bag></dk:TagsList>"#,
            "",
            false,
        ),
        (
            "",
            "<dk:Other><dk:TagsList><rdf:Bag/></dk:TagsList></dk:Other>",
            "",
            false,
        ),
        (
            "",
            "",
            r#"<rdf:Description rdf:about="another.jpg"><dk:TagsList><rdf:Bag/></dk:TagsList></rdf:Description>"#,
            false,
        ),
    ] {
        let (directory, mut archive) = xmp_fixture();
        let path = xmp_original(directory.path(), &mut archive, "photo");
        rusqlite::Connection::open(archive.root.join(".singulo/catalog.sqlite")).unwrap()
            .execute("UPDATE items SET data=json_insert(data,'$.albums[#]','New folder') WHERE id='photo'", [])
            .unwrap();
        let xml = format!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dk="{digikam}" xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:custom="urn:family">
<rdf:Description {attribute} custom:Rating="5">{body}<lr:hierarchicalSubject><rdf:Bag><rdf:li>Existing LR tag</rdf:li></rdf:Bag></lr:hierarchicalSubject></rdf:Description>{extra}</rdf:RDF>"#
        );
        let fallback = archive.root.join(&path).with_extension("xmp");
        fs::write(&fallback, &xml).unwrap();
        let original = fs::read(archive.root.join(&path)).unwrap();
        let summary = archive.export_xmp().unwrap();
        let exported = archive.root.join(format!("{path}.xmp"));
        if supported {
            assert_eq!(summary.created, 1);
            assert!(summary.errors.is_empty(), "{:?}", summary.errors);
            let text = fs::read_to_string(exported).unwrap();
            let doc = roxmltree::Document::parse(&text).unwrap();
            let properties: Vec<_> = doc
                .descendants()
                .filter(|n| n.has_tag_name((digikam, "TagsList")))
                .collect();
            assert_eq!(properties.len(), 1);
            let values: Vec<_> = properties[0]
                .descendants()
                .filter(|n| n.has_tag_name(("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "li")))
                .map(|n| n.text().unwrap())
                .collect();
            assert_eq!(
                values,
                ["Old & treasured", "Family & friends", "Existing LR tag", "New folder"]
            );
            assert!(text.contains("Existing LR tag"));
            assert!(text.contains("custom:Rating=\"5\""));
        } else {
            assert_eq!((summary.created, summary.skipped), (0, 1));
            assert_eq!(summary.errors.len(), 1);
            assert!(
                summary.errors[0].starts_with(&format!("{path}:")),
                "{:?}",
                summary.errors
            );
            assert!(!exported.exists());
        }
        assert_eq!(fs::read_to_string(fallback).unwrap(), xml);
        assert_eq!(fs::read(archive.root.join(&path)).unwrap(), original);
    }
}

#[test]
fn xmp_preserves_fallback_keywords_in_the_effective_tag_collection() {
    let dc = "http://purl.org/dc/elements/1.1/";
    let lr = "http://ns.adobe.com/lightroom/1.0/";
    let dk = "http://www.digikam.org/ns/1.0/";
    for (body, with_digikam, supported) in [
        ("<dc:subject><rdf:Bag><rdf:li>Old &amp; treasured</rdf:li><rdf:li>Family &amp; friends</rdf:li></rdf:Bag></dc:subject>", false, true),
        ("<dc:subject><rdf:Bag><rdf:li>Old &amp; treasured</rdf:li></rdf:Bag></dc:subject>", true, true),
        ("<dc:subject>Old keyword</dc:subject>", false, false),
        ("<dc:subject><rdf:Seq><rdf:li>Old keyword</rdf:li></rdf:Seq></dc:subject>", false, false),
        ("<dc:subject><rdf:Bag><rdf:li rdf:parseType=\"Resource\"/></rdf:Bag></dc:subject>", false, false),
        ("<custom:Other><dc:subject><rdf:Bag/></dc:subject></custom:Other>", false, false),
        ("<iptc:Keywords><rdf:Bag><rdf:li>Old keyword</rdf:li></rdf:Bag></iptc:Keywords>", false, false),
        ("<custom:Other iptc:Keywords=\"Old keyword\"/>", false, false),
        ("<dc:subject><rdf:Bag><rdf:li>Flat|keyword</rdf:li></rdf:Bag></dc:subject>", false, false),
    ] {
        let (directory, mut archive) = xmp_fixture();
        let path = xmp_original(directory.path(), &mut archive, "photo");
        let existing_digikam = if with_digikam {
            "<dk:TagsList><rdf:Bag><rdf:li>DigiKam tag</rdf:li></rdf:Bag></dk:TagsList>"
        } else {
            ""
        };
        let xml = format!(
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dc="{dc}" xmlns:lr="{lr}" xmlns:dk="{dk}" xmlns:custom="urn:family" xmlns:iptc="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"><rdf:Description>{body}{existing_digikam}<lr:hierarchicalSubject><rdf:Bag><rdf:li>Existing|Hierarchy</rdf:li></rdf:Bag></lr:hierarchicalSubject></rdf:Description></rdf:RDF>"#
        );
        let fallback = archive.root.join(&path).with_extension("xmp");
        fs::write(&fallback, &xml).unwrap();
        let summary = archive.export_xmp().unwrap();
        let exported = archive.root.join(format!("{path}.xmp"));
        if supported {
            assert_eq!(summary.created, 1);
            assert!(summary.errors.is_empty(), "{:?}", summary.errors);
            let text = fs::read_to_string(exported).unwrap();
            let doc = roxmltree::Document::parse(&text).unwrap();
            let (namespace, name) = if with_digikam {
                (dk, "TagsList")
            } else {
                (lr, "hierarchicalSubject")
            };
            let property = doc.descendants().find(|n| n.has_tag_name((namespace, name))).unwrap();
            let values: Vec<_> = property.descendants()
                .filter(|n| n.has_tag_name(("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "li")))
                .map(|n| n.text().unwrap()).collect();
            assert!(values.contains(&"Old & treasured"));
            assert_eq!(values.iter().filter(|v| **v == "Family & friends").count(), 1);
            if with_digikam {
                assert!(values.contains(&"DigiKam tag"));
                assert!(values.contains(&"Existing/Hierarchy"));
            } else {
                assert!(values.contains(&"Existing|Hierarchy"));
            }
        } else {
            assert_eq!((summary.created, summary.skipped), (0, 1));
            assert_eq!(summary.errors.len(), 1);
            assert!(summary.errors[0].starts_with(&format!("{path}:")));
            assert!(!exported.exists());
        }
        assert_eq!(fs::read_to_string(fallback).unwrap(), xml);
        assert_eq!(fs::read(archive.root.join(path)).unwrap(), b"photo");
    }
}

#[test]
fn xmp_import_attention_is_persisted_separately_from_import_failures() {
    let (directory, mut archive) = xmp_fixture();
    archive.export_xmp().unwrap();
    let path = xmp_original(directory.path(), &mut archive, "photo");
    fs::write(archive.root.join(format!("{path}.xmp")), b"User's sidecar").unwrap();
    let state = AppState::default();
    *state.archive.lock().unwrap() = Some(archive);
    let mut progress = Progress {
        status: "complete".into(),
        copied: 1,
        ..Progress::default()
    };
    crate::import::sync_xmp_warnings(&state, &mut progress);
    assert!(progress.errors.is_empty());
    assert_eq!(progress.status, "complete");
    assert_eq!(progress.warnings.len(), 1);
    assert!(progress.warnings[0].contains(&path));
    let mut legacy = serde_json::to_value(&progress).unwrap();
    legacy.as_object_mut().unwrap().remove("warnings");
    assert!(serde_json::from_value::<Progress>(legacy).unwrap().warnings.is_empty());
    let guard = state.archive.lock().unwrap();
    let archive = guard.as_ref().unwrap();
    archive.save_job("warning", &serde_json::json!({"id":"warning","progress":progress})).unwrap();
    let jobs = archive.jobs().unwrap();
    let saved: Progress = serde_json::from_value(jobs[0]["progress"].clone()).unwrap();
    assert_eq!(saved.warnings, progress.warnings);
    assert!(saved.errors.is_empty());
    assert_eq!((saved.status.as_str(), saved.copied), ("complete", 1));

    let marker = archive.root.join(".singulo/archive-id");
    let parked = marker.with_extension("saved");
    fs::rename(&marker, &parked).unwrap();
    drop(guard);
    progress.status = "failed".into();
    progress.errors.push("Source copy failed".into());
    crate::import::sync_xmp_warnings(&state, &mut progress);
    assert_eq!(progress.errors, ["Source copy failed"]);
    assert_eq!(progress.status, "failed");
    assert_eq!(progress.warnings.len(), 2);
    assert!(progress.warnings[1].starts_with("Managed XMP export needs attention:"));
    fs::rename(parked, marker).unwrap();
}

#[test]
fn xmp_opening_reports_pending_work_without_draining_the_export_queue() {
    let (directory, mut archive) = xmp_fixture();
    assert_eq!(archive.export_xmp().unwrap().created, 0);
    let paths: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|id| xmp_original(directory.path(), &mut archive, id))
        .collect();
    let root = archive.root.clone();
    drop(archive);
    let mut archive = Archive::open(&root).unwrap();
    let errors = archive.recover().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("pending"));
    assert!(errors[0].contains("Export XMP"));
    for path in &paths {
        assert!(!root.join(format!("{path}.xmp")).exists());
    }
    let connection = rusqlite::Connection::open(root.join(".singulo/catalog.sqlite")).unwrap();
    let dirty: i64 = connection
        .query_row("SELECT COUNT(*) FROM xmp_dirty", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dirty, 2);
    let resumed = archive.xmp_worker().unwrap().sync_exported_xmp().unwrap();
    assert_eq!(resumed.created, 2);
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
}

#[cfg(unix)]
#[test]
fn xmp_export_stats_originals_without_reading_them_and_rejects_non_regular_files() {
    use std::os::unix::fs::PermissionsExt;
    let (directory, mut archive) = xmp_fixture();
    let path = xmp_original(directory.path(), &mut archive, "photo");
    let root = archive.root.clone();
    drop(archive);
    let archive = Archive::open(&root).unwrap();
    let original = root.join(&path);
    let permissions = fs::metadata(&original).unwrap().permissions();
    fs::set_permissions(&original, fs::Permissions::from_mode(0o000)).unwrap();
    let result = archive.export_xmp();
    fs::set_permissions(&original, permissions).unwrap();
    let summary = result.unwrap();
    assert_eq!(summary.created, 1);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert_eq!(fs::read(&original).unwrap(), b"photo");
    assert_eq!(archive.remove_exported_xmp().unwrap().removed, 1);
    fs::remove_file(&original).unwrap();
    fs::create_dir(&original).unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!(summary.created, 0);
    assert!(summary.errors.iter().any(|e| e.contains("regular file")));
    fs::remove_dir(&original).unwrap();
    let source = directory.path().join("photo.jpg").canonicalize().unwrap();
    std::os::unix::fs::symlink(&source, &original).unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!(summary.created, 0);
    assert!(summary.errors.iter().any(|e| e.contains("symlink")));
    assert_eq!(fs::read(source).unwrap(), b"photo");
}

#[test]
fn xmp_lifecycle_tracks_new_imports_dates_reorganization_undo_and_albums() {
    let (directory, archive) = xmp_fixture();
    assert_eq!(archive.export_xmp().unwrap().created, 0);
    let root = archive.root.clone();
    drop(archive);
    let mut archive = Archive::open(&root).unwrap();
    assert!(archive.recover().unwrap().is_empty());
    let initial = xmp_original(directory.path(), &mut archive, "photo");
    let sync = archive.sync_exported_xmp().unwrap();
    assert_eq!(sync.created, 1);
    archive.correct_date("photo", "2023-07-19").unwrap();
    let sync = archive.sync_exported_xmp().unwrap();
    assert_eq!(sync.updated, 1);
    assert!(sync.errors.is_empty(), "{:?}", sync.errors);
    assert!(!archive.root.join(format!("{initial}.xmp")).exists());
    let corrected = archive.item("photo").unwrap();
    let corrected_sidecar = format!("{}.xmp", corrected.path);
    assert!(
        fs::read_to_string(archive.root.join(&corrected_sidecar))
            .unwrap()
            .contains("2023-07-19T14:32:10")
    );
    let source = directory.path().join("photo.jpg").canonicalize().unwrap();
    archive
        .record_occurrence(&Occurrence {
            computer: None,
            id: "extra".into(),
            item: "photo".into(),
            source: source.to_string_lossy().into(),
            root: directory.path().to_string_lossy().into(),
            relative: "scan/photo.jpg".into(),
            album: "New album".into(),
            hash: corrected.media.hash.clone(),
            identity: media::identity(&source).unwrap(),
            status: "pending".into(),
            bytes: corrected.media.bytes,
        })
        .unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().updated, 1);
    assert!(
        fs::read_to_string(archive.root.join(&corrected_sidecar))
            .unwrap()
            .contains("New album")
    );
    let mut settings = archive.settings.clone();
    settings.template = "Family/{yyyy}".into();
    archive.save_settings(settings).unwrap();
    let preview = archive.move_preview(&[]).unwrap();
    archive.reorganize(&preview).unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().updated, 1);
    assert!(!archive.root.join(&corrected_sidecar).exists());
    let rollback = archive.rollback_preview().unwrap();
    archive.rollback(&rollback).unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().updated, 1);
    assert!(archive.root.join(&corrected_sidecar).exists());
    xmp_original(directory.path(), &mut archive, "keeper");
    archive.sync_exported_xmp().unwrap();
    archive.move_to_undo("photo", "keeper").unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().removed, 1);
    let undo = archive.item("photo").unwrap().path;
    assert!(!archive.root.join(format!("{undo}.xmp")).exists());
    archive.restore("photo").unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().created, 1);
    assert!(!archive.root.join(format!("{undo}.xmp")).exists());
    let active_sidecar = archive
        .root
        .join(format!("{}.xmp", archive.item("photo").unwrap().path));
    let generated = fs::read(&active_sidecar).unwrap();
    let modified = [generated.as_slice(), b"<!-- user's edit -->"].concat();
    fs::write(&active_sidecar, &modified).unwrap();
    archive.move_to_undo("photo", "keeper").unwrap();
    let sync = archive.sync_exported_xmp().unwrap();
    assert_eq!(sync.removed, 0);
    assert!(sync.errors.iter().any(|e| e.contains("Modified")));
    assert_eq!(fs::read(&active_sidecar).unwrap(), modified);
    assert!(
        !archive
            .root
            .join(format!("{}.xmp", archive.item("photo").unwrap().path))
            .exists()
    );
    archive.restore("photo").unwrap();
    assert!(!archive.sync_exported_xmp().unwrap().errors.is_empty());
    fs::write(&active_sidecar, generated).unwrap();
    assert!(archive.sync_exported_xmp().unwrap().errors.is_empty());
    archive.move_to_undo("photo", "keeper").unwrap();
    assert_eq!(archive.sync_exported_xmp().unwrap().removed, 1);
    let undo = archive.item("photo").unwrap().path;
    archive.purge("photo", true).unwrap();
    let sync = archive.sync_exported_xmp().unwrap();
    assert_eq!(sync.removed, 0);
    assert!(sync.errors.is_empty(), "{:?}", sync.errors);
    assert!(!archive.root.join(format!("{undo}.xmp")).exists());
    xmp_original(directory.path(), &mut archive, "never-exported");
    archive
        .relocate(
            "never-exported",
            ".singulo/undo/never-exported/photo.jpg".into(),
            "undo",
            None,
        )
        .unwrap();
    let sync = archive.export_xmp().unwrap();
    assert_eq!(sync.created, 0);
    assert!(sync.errors.is_empty(), "{:?}", sync.errors);
    assert!(
        !archive
            .root
            .join(format!(
                "{}.xmp",
                archive.item("never-exported").unwrap().path
            ))
            .exists()
    );
    assert_eq!(fs::read(source).unwrap(), b"photo");
}

#[test]
fn xmp_recovers_parked_updates_and_retains_all_bytes_on_recovery_collision() {
    for (collide, inactive) in [(false, false), (true, false), (false, true)] {
        let (directory, mut archive) = xmp_fixture();
        let path = xmp_original(directory.path(), &mut archive, "photo");
        assert_eq!(archive.export_xmp().unwrap().created, 1);
        let connection =
            rusqlite::Connection::open(archive.root.join(".singulo/catalog.sqlite")).unwrap();
        let data: String = connection
            .query_row(
                "SELECT data FROM exported_xmp WHERE item='photo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let old: serde_json::Value = serde_json::from_str(&data).unwrap();
        let mut new = old.clone();
        let sidecar = archive.root.join(format!("{path}.xmp"));
        let text = fs::read_to_string(&sidecar)
            .unwrap()
            .replace("Family &amp; friends", "Revised");
        let stage = ".singulo/staging/xmp-test";
        fs::write(archive.root.join(stage), &text).unwrap();
        new["hash"] = media::hash(&archive.root.join(stage)).unwrap().into();
        let parked = ".singulo/recovery/xmp-test";
        fs::rename(&sidecar, archive.root.join(parked)).unwrap();
        if collide {
            fs::write(&sidecar, b"Somebody else's new sidecar").unwrap();
        }
        if inactive {
            archive
                .relocate(
                    "photo",
                    ".singulo/undo/photo/photo.jpg".into(),
                    "undo",
                    None,
                )
                .unwrap();
        }
        let pending = serde_json::json!({"old":old,"new":new,"stage":stage,"parked":parked,"committed":false});
        connection
            .execute(
                "INSERT INTO xmp_operations VALUES('photo',?1)",
                [pending.to_string()],
            )
            .unwrap();
        drop(connection);
        let root = archive.root.clone();
        drop(archive);
        let mut archive = Archive::open(&root).unwrap();
        let attention = archive.recover().unwrap();
        assert_eq!(attention.len(), 1);
        assert!(attention[0].contains("pending"));
        assert!(archive.root.join(stage).exists());
        assert!(archive.root.join(parked).exists());
        let errors = archive
            .xmp_worker()
            .unwrap()
            .sync_exported_xmp()
            .unwrap()
            .errors;
        if collide {
            assert!(!errors.is_empty());
            assert!(errors[0].starts_with(&format!("{path}:")));
            assert_eq!(fs::read(&sidecar).unwrap(), b"Somebody else's new sidecar");
            assert!(archive.root.join(stage).exists());
            assert!(archive.root.join(parked).exists());
        } else if inactive {
            assert!(errors.is_empty(), "{errors:?}");
            assert!(!sidecar.exists());
            assert!(!archive.root.join(stage).exists());
            assert!(!archive.root.join(parked).exists());
            assert!(
                !archive
                    .root
                    .join(format!("{}.xmp", archive.item("photo").unwrap().path))
                    .exists()
            );
        } else {
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(fs::read_to_string(&sidecar).unwrap(), text);
            assert!(!archive.root.join(stage).exists());
            assert!(!archive.root.join(parked).exists());
        }
        assert_eq!(
            fs::read(directory.path().join("photo.jpg")).unwrap(),
            b"photo"
        );
    }
}

#[cfg(unix)]
#[test]
fn xmp_refuses_symlink_sidecars_and_unmergeable_metadata() {
    let (directory, mut archive) = xmp_fixture();
    let path = xmp_original(directory.path(), &mut archive, "photo");
    fs::write(directory.path().join("other.xmp"), b"keep me").unwrap();
    let outside = directory.path().join("other.xmp").canonicalize().unwrap();
    let fallback = archive.root.join(&path).with_extension("xmp");
    std::os::unix::fs::symlink(&outside, &fallback).unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!((summary.created, summary.skipped), (0, 1));
    assert!(summary.errors.iter().any(|e| e.contains("symlink")));
    assert_eq!(fs::read(&outside).unwrap(), b"keep me");
    fs::remove_file(&fallback).unwrap();
    let unsupported = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:subject rdf:parseType="Resource"><dc:unknown>preserve</dc:unknown></dc:subject></rdf:Description></rdf:RDF>"#;
    fs::write(&fallback, unsupported).unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!(summary.created, 0);
    assert!(!summary.errors.is_empty());
    assert_eq!(fs::read_to_string(fallback).unwrap(), unsupported);
    assert!(!archive.root.join(format!("{path}.xmp")).exists());
    let preferred = archive.root.join(format!("{path}.xmp"));
    std::os::unix::fs::symlink(&outside, &preferred).unwrap();
    let summary = archive.export_xmp().unwrap();
    assert_eq!(summary.created, 0);
    assert!(summary.errors.iter().any(|e| e.contains("symlink")));
    assert_eq!(fs::read(outside).unwrap(), b"keep me");
}

#[test]
fn dates_use_metadata_then_mark_estimates_and_unknown() {
    let metadata = serde_json::json!({"EXIF:DateTimeOriginal":"1965:03:04 12:13:14"});
    assert_eq!(
        media::select_date(&metadata, None, None).0.as_deref(),
        Some("1965-03-04T12:13:14")
    );
    let default = serde_json::json!({"EXIF:DateTimeOriginal":"1970:01:01 00:00:00"});
    let selected = media::select_date(&default, None, Some("2023-07-18T14:32:10"));
    assert_eq!(selected.1, "estimated: file modification");
    assert!(media::select_date(&default, None, None).0.is_none());
}

#[test]
fn path_dates_precede_filesystem_dates_without_overriding_capture_dates() {
    let mut media = sample(String::new());
    media.date_source = "estimated: filesystem creation".into();
    media::apply_path_date(&mut media, "IMG-20170114-WA0005.jpg", Some("2020-03-04"));
    assert_eq!(media.date.as_deref(), Some("2017-01-14"));
    assert_eq!(media.date_source, "estimated: filename date");
    assert_eq!(
        paths::relative(&Settings::default(), &media, "IMG-20170114-WA0005.jpg").unwrap(),
        Path::new("2017/01/14/IMG-20170114-WA0005.jpg")
    );

    media.date_source = "capture metadata".into();
    media::apply_path_date(&mut media, "2024-07-18.jpg", Some("2020-03-04"));
    assert_eq!(media.date.as_deref(), Some("2017-01-14"));

    for filename in [
        "IMG-20230230-WA0005.jpg",
        "IMG-19700101.jpg",
        "IMG-99991231.jpg",
        "IMG-1201701149.jpg",
    ] {
        media.date_source = "estimated: file modification".into();
        media::apply_path_date(&mut media, filename, Some("2020-03-04"));
        assert_eq!(media.date.as_deref(), Some("2020-03-04"), "{filename}");
        assert_eq!(media.date_source, "estimated: folder date");
    }

    media.date = None;
    media.date_source = "unknown".into();
    media.warnings = vec!["No usable date. Set the date manually if known.".into()];
    media::apply_path_date(&mut media, "photo_2024-07-18.jpg", None);
    assert_eq!(media.date.as_deref(), Some("2024-07-18"));
    assert!(media.warnings.is_empty());

    media.date_source = "estimated: file modification".into();
    media::apply_path_date(&mut media, "photo_2024-7-8.jpg", None);
    assert_eq!(media.date.as_deref(), Some("2024-07-08"));

    media.date = Some("2023-07-18T14:32:10".into());
    media.date_source = "estimated: filesystem creation".into();
    media::apply_path_date(&mut media, "photo.jpg", None);
    assert_eq!(media.date.as_deref(), Some("2023-07-18T14:32:10"));
    assert_eq!(media.date_source, "estimated: filesystem creation");
}

#[test]
fn folder_dates_are_fallbacks_and_date_parts_are_not_folder_labels() {
    let parts = ["Photos", "2024", "07", "18", "Italy", "Day 1"].map(str::to_string);
    assert_eq!(
        media::folder_path_info(&parts),
        (
            Some("2024-07-18".into()),
            ["Photos", "Italy", "Day 1"].map(str::to_string).to_vec()
        )
    );
    let invalid = ["Photos", "2024", "02", "30", "Italy"].map(str::to_string);
    assert_eq!(
        media::folder_path_info(&invalid),
        (None, ["Photos", "Italy"].map(str::to_string).to_vec())
    );
    assert_eq!(media::folder_path_info(&["1970-01-01".into()]).0, None);

    let (directory, mut archive) = xmp_fixture();
    let add_item = |archive: &mut Archive, id: &str, date: Option<&str>| {
        let source = directory.path().join(format!("{id}.jpg"));
        fs::write(&source, id.as_bytes()).unwrap();
        let hash = media::hash(&source).unwrap();
        let mut item_media = sample(hash.clone());
        item_media.date = date.map(str::to_string);
        item_media.date_source = if date.is_some() {
            "capture metadata".into()
        } else {
            "unknown".into()
        };
        if date.is_none() {
            item_media
                .warnings
                .push("No usable date. Set the date manually if known.".into());
        }
        archive
            .place(
                &source,
                Item {
                    id: id.into(),
                    path: format!("{id}.jpg"),
                    original_name: format!("{id}.jpg"),
                    state: "active".into(),
                    keeper: None,
                    media: item_media,
                    settings: Settings::default(),
                    albums: vec![],
                },
                None,
                None,
            )
            .unwrap();
        hash
    };

    for (id, original_date, expected_date, relative) in [
        (
            "folder-dated",
            None,
            "2024-07-18",
            "2024/07/18/Italy/Day 1/folder-dated.jpg",
        ),
        (
            "capture-dated",
            Some("2019-05-06"),
            "2019-05-06",
            "2024/07/18/Italy/capture-dated.jpg",
        ),
    ] {
        let hash = add_item(&mut archive, id, original_date);
        archive
            .record_occurrence(&Occurrence {
            computer: None,
                id: format!("source-{id}"),
                item: id.into(),
                source: format!("/source/Photos/{relative}"),
                root: "/source/Photos".into(),
                album: "Photos".into(),
                relative: relative.into(),
                hash,
                identity: "fixture".into(),
                status: "pending".into(),
                bytes: id.len() as u64,
            })
            .unwrap();
        let item = archive.item(id).unwrap();
        assert_eq!(item.media.date.as_deref(), Some(expected_date));
        if original_date.is_none() {
            assert_eq!(item.media.date_source, "estimated: folder date");
            assert_eq!(
                item.albums,
                ["Photos/Italy/Day 1", "Photos", "Italy", "Day 1"]
            );
            assert!(
                !item
                    .media
                    .warnings
                    .contains(&"No usable date. Set the date manually if known.".into())
            );
        } else {
            assert_eq!(item.media.date_source, "capture metadata");
            assert_eq!(item.albums, ["Photos/Italy", "Photos", "Italy"]);
        }
    }
}

#[test]
fn repair_estimated_dates_moves_safely_and_preserves_protected_dates() {
    let (directory, mut archive) = xmp_fixture();
    let add =
        |archive: &mut Archive, id: &str, name: &str, kind: &str, source: &str, relative: &str| {
            let file = directory.path().join(source);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, id.as_bytes()).unwrap();
            let hash = media::hash(&file).unwrap();
            let mut item_media = sample(hash.clone());
            item_media.kind = kind.into();
            item_media.date_source = match id {
                "capture" => "capture metadata",
                "folder-priority" => "estimated: folder date",
                _ => "estimated: filesystem creation",
            }
            .into();
            archive
                .place(
                    &file,
                    Item {
                        id: id.into(),
                        path: format!("2023/07/18/{name}"),
                        original_name: name.into(),
                        state: "active".into(),
                        keeper: None,
                        media: item_media,
                        settings: Settings::default(),
                        albums: vec![],
                    },
                    Some(Occurrence {
            computer: None,
                        id: format!("occ-{id}"),
                        item: id.into(),
                        source: file.to_string_lossy().into(),
                        root: directory.path().to_string_lossy().into(),
                        album: "Photos".into(),
                        relative: relative.into(),
                        hash,
                        identity: media::identity(&file).unwrap(),
                        status: "pending".into(),
                        bytes: id.len() as u64,
                    }),
                    None,
                )
                .unwrap();
        };
    add(
        &mut archive,
        "photo",
        "IMG-20170114-WA0005.jpg",
        "photo",
        "source/IMG-20170114-WA0005.jpg",
        "IMG-20170114-WA0005.jpg",
    );
    add(
        &mut archive,
        "sidecar",
        "IMG-20170114-WA0005.jpg.xmp",
        "sidecar",
        "source/IMG-20170114-WA0005.jpg.xmp",
        "IMG-20170114-WA0005.jpg.xmp",
    );
    add(
        &mut archive,
        "folder",
        "folder.jpg",
        "photo",
        "source/folder.jpg",
        "2020/03/04/folder.jpg",
    );
    add(
        &mut archive,
        "folder-priority",
        "IMG-20160101-pref.jpg",
        "photo",
        "source/IMG-20160101-pref.jpg",
        "IMG-20160101-pref.jpg",
    );
    add(
        &mut archive,
        "conflict",
        "conflict.jpg",
        "photo",
        "source/conflict.jpg",
        "2020/03/04/conflict.jpg",
    );
    let conflict = archive.item("conflict").unwrap();
    archive
        .record_occurrence(&Occurrence {
            computer: None,
            id: "other-conflict".into(),
            item: "conflict".into(),
            source: "another/conflict.jpg".into(),
            root: "another".into(),
            album: "Photos".into(),
            relative: "2021\\04\\05\\conflict.jpg".into(),
            hash: conflict.media.hash,
            identity: "fixture".into(),
            status: "pending".into(),
            bytes: 8,
        })
        .unwrap();
    add(
        &mut archive,
        "manual",
        "20170114-manual.jpg",
        "photo",
        "source/20170114-manual.jpg",
        "20170114-manual.jpg",
    );
    archive.correct_date("manual", "2018-01-02").unwrap();
    add(
        &mut archive,
        "capture",
        "20170114-capture.jpg",
        "photo",
        "source/20170114-capture.jpg",
        "20170114-capture.jpg",
    );
    add(
        &mut archive,
        "shared-jpeg",
        "IMG-20180101.jpg",
        "photo",
        "source/IMG-20180101.jpg",
        "IMG-20180101.jpg",
    );
    add(
        &mut archive,
        "shared-raw",
        "IMG-20180101.CR2",
        "raw",
        "source/IMG-20180101.CR2",
        "IMG-20180101.CR2",
    );
    add(
        &mut archive,
        "shared-xmp",
        "IMG-20180101.jpg.xmp",
        "sidecar",
        "source/IMG-20180101.jpg.xmp",
        "IMG-20180101.jpg.xmp",
    );

    let occupied = directory
        .path()
        .join("archive/2017/01/14/IMG-20170114-WA0005.jpg");
    fs::create_dir_all(occupied.parent().unwrap()).unwrap();
    fs::write(&occupied, b"unmanaged").unwrap();
    let summary = serde_json::to_value(archive.repair_estimated_dates().unwrap()).unwrap();
    assert_eq!(summary["updated"], 3);
    assert_eq!(summary["skipped"], 3);
    assert_eq!(summary["errors"], serde_json::json!([]));
    let photo = archive.item("photo").unwrap();
    assert_eq!(photo.media.date.as_deref(), Some("2017-01-14"));
    assert_eq!(photo.media.date_source, "estimated: filename date");
    assert_eq!(photo.path, "2017/01/14/IMG-20170114-WA0005__photo.jpg");
    assert_eq!(fs::read(&occupied).unwrap(), b"unmanaged");
    assert_eq!(
        fs::read(directory.path().join("archive").join(&photo.path)).unwrap(),
        b"photo"
    );
    let sidecar = archive.item("sidecar").unwrap();
    assert_eq!(sidecar.path, "2017/01/14/IMG-20170114-WA0005__photo.xmp");
    assert_eq!(sidecar.media.date, photo.media.date);
    assert_eq!(
        archive.item("folder").unwrap().path,
        "2020/03/04/folder.jpg"
    );
    assert_eq!(
        archive.item("folder-priority").unwrap().path,
        "2016/01/01/IMG-20160101-pref.jpg"
    );
    assert_eq!(
        archive.item("conflict").unwrap().media.date_source,
        "estimated: filesystem creation"
    );
    assert_eq!(
        archive.item("shared-jpeg").unwrap().path,
        "2023/07/18/IMG-20180101.jpg"
    );
    assert_eq!(
        archive.item("shared-raw").unwrap().path,
        "2023/07/18/IMG-20180101.CR2"
    );
    assert_eq!(
        archive.item("shared-xmp").unwrap().path,
        "2023/07/18/IMG-20180101.jpg.xmp"
    );
    assert_eq!(
        archive.item("manual").unwrap().media.date.as_deref(),
        Some("2018-01-02T14:32:10")
    );
    assert_eq!(
        archive.item("capture").unwrap().media.date.as_deref(),
        Some("2023-07-18T14:32:10")
    );
    assert_eq!(
        fs::read(directory.path().join("source/IMG-20170114-WA0005.jpg")).unwrap(),
        b"photo"
    );
    let again = serde_json::to_value(archive.repair_estimated_dates().unwrap()).unwrap();
    assert_eq!(again["updated"], 0);
    assert_eq!(again["skipped"], 3);

    add(
        &mut archive,
        "bad",
        "IMG-20190102-bad.jpg",
        "photo",
        "source/IMG-20190102-bad.jpg",
        "IMG-20190102-bad.jpg",
    );
    let original = directory.path().join("archive/2023/07/18/IMG-20190102-bad.jpg");
    fs::write(&original, b"changed").unwrap();
    let failed = serde_json::to_value(archive.repair_estimated_dates().unwrap()).unwrap();
    assert_eq!(failed["updated"], 0);
    assert!(!failed["errors"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(&original).unwrap(), b"changed");
    fs::write(&original, b"bad").unwrap();
    let retried = serde_json::to_value(archive.repair_estimated_dates().unwrap()).unwrap();
    assert_eq!(retried["updated"], 1);
    assert_eq!(archive.item("bad").unwrap().path, "2019/01/02/IMG-20190102-bad.jpg");
}

#[test]
fn catalog_upgrade_preserves_legacy_sources_and_separates_computers() {
    let (directory, mut archive) = xmp_fixture();
    xmp_original(directory.path(), &mut archive, "photo");
    xmp_original(directory.path(), &mut archive, "other");
    let hash = archive.item("photo").unwrap().media.hash;
    let legacy = Occurrence {
        id: "legacy".into(),
        item: "photo".into(),
        source: "/source/photo.jpg".into(),
        root: "/source".into(),
        album: "source".into(),
        relative: "photo.jpg".into(),
        hash: hash.clone(),
        identity: "fixture".into(),
        status: "pending".into(),
        bytes: 5,
        computer: None,
    };
    archive.record_occurrence(&legacy).unwrap();
    drop(archive);
    let catalog = directory.path().join("archive/.singulo/catalog.sqlite");
    let connection = rusqlite::Connection::open(&catalog).unwrap();
    connection.execute_batch(
        "BEGIN;
         CREATE TABLE occurrences_v3(id TEXT PRIMARY KEY, source TEXT NOT NULL, hash TEXT NOT NULL, data TEXT NOT NULL, UNIQUE(source,hash));
         INSERT INTO occurrences_v3 SELECT id,source,hash,data FROM occurrences;
         DROP TABLE occurrences;
         ALTER TABLE occurrences_v3 RENAME TO occurrences;
         CREATE TABLE source_groups_v3(source TEXT PRIMARY KEY, item TEXT NOT NULL, group_key TEXT NOT NULL, content_id TEXT);
         INSERT INTO source_groups_v3 SELECT source,item,group_key,content_id FROM source_groups;
         DROP TABLE source_groups;
         ALTER TABLE source_groups_v3 RENAME TO source_groups;
         PRAGMA user_version=3;
         COMMIT;"
    ).unwrap();
    drop(connection);
    let mut archive = Archive::open(&directory.path().join("archive")).unwrap();
    assert!(archive.occurrence("legacy").unwrap().computer.is_none());
    assert!(fs::read_dir(archive.root.join(".singulo/recovery")).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if !path.file_name().unwrap().to_string_lossy().starts_with("before-computer-migration-") {
            return false;
        }
        let backup = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let version: u32 = backup.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        let occurrences: u32 = backup.query_row("SELECT count(*) FROM occurrences", [], |row| row.get(0)).unwrap();
        version == 3 && occurrences == 1
    }));
    for (id, name) in [("machine-a", "Laptop A"), ("machine-b", "Laptop B")] {
        let mut occurrence = legacy.clone();
        occurrence.id = id.into();
        occurrence.computer = Some(Computer { id: id.into(), name: name.into() });
        archive.record_occurrence(&occurrence).unwrap();
        assert_eq!(archive.occurrence_id(Path::new("/source/photo.jpg"), &hash, id).unwrap().as_deref(), Some(id));
    }
    assert_eq!(archive.item_occurrences("photo").unwrap().len(), 3);

    let mut other = legacy.clone();
    other.id = "other-source".into();
    other.item = "other".into();
    other.hash = archive.item("other").unwrap().media.hash;
    other.computer = Some(Computer { id: "machine-b".into(), name: "Laptop B".into() });
    archive.record_occurrence(&other).unwrap();
    assert_eq!(archive.sidecar_owner(Path::new("/source/photo.xmp"), "machine-a").unwrap().unwrap().id, "photo");
    assert_eq!(archive.sidecar_owner(Path::new("/source/photo.xmp"), "machine-b").unwrap().unwrap().id, "other");
}

#[test]
fn remembered_archive_requires_matching_identity_and_existing_catalog() {
    let (directory, archive) = xmp_fixture();
    let root = archive.root.clone();
    let id = fs::read_to_string(root.join(".singulo/archive-id")).unwrap();
    drop(archive);
    assert!(Archive::open_existing(&root, "wrong-id").is_err());
    assert!(Archive::open_existing(&root, &id).is_ok());
    let catalog = root.join(".singulo/catalog.sqlite");
    fs::rename(&catalog, root.join(".singulo/catalog.saved")).unwrap();
    assert!(Archive::open_existing(&root, &id).is_err());
    assert!(!catalog.exists());
    let unrelated = directory.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    assert!(Archive::open_existing(&unrelated, &id).is_err());
    assert!(!unrelated.join(".singulo").exists());
}

#[test]
fn folder_search_excludes_archive_and_never_touches_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = root.join("pictures");
    let archive_root = root.join("archive");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&archive_root).unwrap();
    drop(Archive::open(&archive_root).unwrap());
    fs::write(source.join("one.jpg"), b"original").unwrap();
    fs::write(source.join("two.MOV"), b"video").unwrap();
    fs::write(source.join("one.xmp"), b"sidecar").unwrap();
    fs::write(root.join("root.jpg"), b"root original").unwrap();
    fs::write(archive_root.join("archived.jpg"), b"archived").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&archive_root, root.join("archive-link")).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let status = crate::discovery::scan(
        &root.to_string_lossy(),
        Some(&archive_root),
        &cancel,
        |_| Ok(()),
    ).unwrap();
    let status = serde_json::to_value(status).unwrap();
    let folders = status["folders"].as_array().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0]["path"], fs::canonicalize(&source).unwrap().to_string_lossy().as_ref());
    assert_eq!(folders[0]["count"], 2);
    assert_eq!(status["excluded"], 1);
    assert_eq!(fs::read(source.join("one.jpg")).unwrap(), b"original");
    assert_eq!(fs::read(archive_root.join("archived.jpg")).unwrap(), b"archived");
    assert!(crate::discovery::scan(&archive_root.to_string_lossy(), Some(&archive_root), &cancel, |_| Ok(())).is_err());
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let stopped = crate::discovery::scan(&root.to_string_lossy(), Some(&archive_root), &cancel, |_| Ok(())).unwrap();
    assert!(stopped.cancelled);
    assert!(stopped.folders.is_empty());
}

#[cfg(windows)]
#[test]
fn windows_archive_overlap_ignores_path_case_and_respects_components() {
    assert!(paths::starts_with(Path::new(r"c:\Photos\Archive\2024"), Path::new(r"C:\photos\archive")));
    assert!(!paths::starts_with(Path::new(r"C:\photos\archive-copy"), Path::new(r"C:\photos\archive")));
}

#[test]
fn portable_archive_location_moves_with_app_and_checks_identity() {
    let directory = tempfile::tempdir().unwrap();
    let disk = directory.path().join("disk");
    let app = disk.join("app");
    let root = disk.join("photos");
    fs::create_dir_all(&app).unwrap();
    fs::create_dir(&root).unwrap();
    drop(Archive::open(&root).unwrap());
    let value = crate::portable::archive_value_at(&app, &root).unwrap();
    let moved = directory.path().join("moved");
    fs::rename(&disk, &moved).unwrap();
    let (path, id) = crate::portable::resolve_archive_at(&moved.join("app"), &value).unwrap();
    assert!(Archive::open_existing(Path::new(&path), &id).is_ok());
    fs::write(moved.join("photos/.singulo/archive-id"), "different").unwrap();
    assert!(Archive::open_existing(Path::new(&path), &id).is_err());
    let incompatible = r#"{"relative":false,"path":"/other/photos","os":"other","archive_id":"id"}"#;
    assert!(crate::portable::resolve_archive_at(&moved.join("app"), incompatible).is_err());
}

#[test]
fn interrupted_cleanup_does_not_touch_foreign_source_or_staging_files() {
    let (directory, mut archive) = xmp_fixture();
    xmp_original(directory.path(), &mut archive, "photo");
    let source = directory.path().join("source.jpg");
    let staged = directory.path().join("staged.jpg");
    fs::write(&source, b"source").unwrap();
    fs::write(&staged, b"staged").unwrap();
    let hash = archive.item("photo").unwrap().media.hash;
    archive.record_occurrence(&Occurrence {
        id: "interrupted".into(),
        item: "photo".into(),
        source: source.to_string_lossy().into(),
        root: directory.path().to_string_lossy().into(),
        album: "source".into(),
        relative: "source.jpg".into(),
        hash,
        identity: "fixture".into(),
        status: format!("staged:{}", staged.display()),
        bytes: 5,
        computer: Some(Computer { id: "another-computer".into(), name: "Other".into() }),
    }).unwrap();
    let warnings = archive.recover().unwrap();
    assert!(warnings.iter().any(|warning| warning.contains(&staged.to_string_lossy().to_string())));
    assert_eq!(fs::read(source).unwrap(), b"source");
    assert_eq!(fs::read(staged).unwrap(), b"staged");
    assert!(archive.occurrence("interrupted").unwrap().status.starts_with("staged:"));
}

#[test]
fn layout_renders_dates_and_blocks_escaping_paths() {
    let mut settings = Settings::default();
    let media = sample(String::new());
    assert_eq!(
        paths::relative(&settings, &media, "IMG.jpg").unwrap(),
        Path::new("2023/07/18/IMG.jpg")
    );
    settings.template = "Family/{yyyy}/{mm}".into();
    assert_eq!(
        paths::relative(&settings, &media, "IMG.jpg").unwrap(),
        Path::new("Family/2023/07/IMG.jpg")
    );
    settings.template = "../{yyyy}".into();
    assert!(paths::validate(&settings).is_err());
    settings.template = "{anything}".into();
    assert!(paths::validate(&settings).is_err());
}

#[test]
fn exact_duplicates_keep_provenance_and_require_present_keeper() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let target = directory.path().join("archive");
    fs::create_dir(&target).unwrap();
    fs::write(&source, b"test").unwrap();
    let source = source.canonicalize().unwrap();
    let hash = media::hash(&source).unwrap();
    let mut archive = Archive::open(&target).unwrap();
    let item = Item {
        id: "original".into(),
        path: "2023/07/18/IMG.jpg".into(),
        original_name: "IMG.jpg".into(),
        state: "active".into(),
        keeper: None,
        media: sample(hash.clone()),
        settings: Settings::default(),
        albums: vec!["Holiday".into()],
    };
    archive.place(&source, item, None, None).unwrap();
    archive
        .record_occurrence(&Occurrence {
            computer: None,
            id: "backup".into(),
            item: "original".into(),
            source: source.to_string_lossy().into(),
            root: directory.path().to_string_lossy().into(),
            relative: "source".into(),
            album: "Old backup".into(),
            hash: hash.clone(),
            identity: media::identity(&source).unwrap(),
            status: "pending".into(),
            bytes: 4,
        })
        .unwrap();
    assert_eq!(
        archive.exact(&hash).unwrap().unwrap().albums,
        ["Holiday", "Old backup"]
    );
    assert_eq!(archive.totals("active").unwrap(), (1, 4, 1));
    assert_eq!(archive.page("old backup", "all", None, 0).unwrap().len(), 1);
    let mut occurrence = archive.item_occurrences("original").unwrap().remove(0);
    occurrence.album = "Été".into();
    archive.record_occurrence(&occurrence).unwrap();
    assert_eq!(archive.page("été", "all", None, 0).unwrap().len(), 1);
    let mut batched = occurrence.clone();
    batched.id = "batched".into();
    batched.source = directory
        .path()
        .join("another-source.jpg")
        .to_string_lossy()
        .into();
    batched.album = "Uncommitted album".into();
    let mut invalid = batched.clone();
    invalid.id = "invalid".into();
    invalid.source = directory
        .path()
        .join("invalid-source.jpg")
        .to_string_lossy()
        .into();
    invalid.item = "missing-item".into();
    assert!(
        archive
            .record_occurrences(&[batched.clone(), invalid])
            .is_err()
    );
    assert!(archive.occurrence("batched").is_err());
    assert!(
        archive
            .page("uncommitted album", "all", None, 0)
            .unwrap()
            .is_empty()
    );
    archive.record_occurrences(&[batched]).unwrap();
    assert!(archive.occurrence("batched").is_ok());
    let wanted = Path::new("2023/07/18/New.jpg");
    assert_eq!(archive.unique_import_path(wanted, "first").unwrap(), wanted);
    assert_eq!(
        archive
            .unique_import_path(Path::new("2023/07/18/new.jpg"), "second")
            .unwrap(),
        Path::new("2023/07/18/new__second.jpg")
    );
    fs::remove_file(target.join("2023/07/18/IMG.jpg")).unwrap();
    assert!(archive.exact(&hash).unwrap().is_none());
}

#[test]
fn cleanup_requires_unchanged_source_and_exact_archived_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.jpg");
    let target = directory.path().join("archive");
    fs::create_dir(&target).unwrap();
    let mut bytes = vec![0x5a; 128 * 1024 + 17];
    fs::write(&source, &bytes).unwrap();
    let source = source.canonicalize().unwrap();
    let hash = blake3::hash(&bytes).to_hex().to_string();
    assert_eq!(
        media::hash_with_identity(&source, &Default::default())
            .unwrap()
            .1,
        hash
    );
    let mut archive = Archive::open(&target).unwrap();
    let mut media = sample(hash.clone());
    media.bytes = bytes.len() as u64;
    let mut item = Item {
        id: "original".into(),
        path: "IMG.jpg".into(),
        original_name: "IMG.jpg".into(),
        state: "active".into(),
        keeper: None,
        media,
        settings: Settings::default(),
        albums: vec![],
    };
    archive.place(&source, item.clone(), None, None).unwrap();
    let occurrence = Occurrence {
            computer: None,
        id: "source".into(),
        item: "original".into(),
        source: source.to_string_lossy().into(),
        root: directory
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into(),
        relative: "source.jpg".into(),
        album: "Source".into(),
        hash,
        identity: media::identity(&source).unwrap(),
        status: "pending".into(),
        bytes: bytes.len() as u64,
    };
    assert!(archive.cleanup_check(&occurrence).is_ok());
    archive.record_occurrence(&occurrence).unwrap();
    let reader = archive.cleanup_reader().unwrap();
    assert!(reader.cleanup_rows().unwrap()[0].eligible);
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(target.join("IMG.jpg"), &bytes).unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
    assert!(!reader.cleanup_rows().unwrap()[0].eligible);
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(target.join("IMG.jpg"), &bytes).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&source, &bytes).unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
    item.id = "changed".into();
    item.path = "changed.jpg".into();
    assert!(
        archive
            .place(&source, item, None, None)
            .unwrap_err()
            .contains("Copied bytes failed verification")
    );
    assert!(!target.join("changed.jpg").exists());
    assert_eq!(fs::read(&source).unwrap(), bytes);
}

fn import_copy_fixture(
    archive: &mut Archive,
    directory: &Path,
    name: &str,
    job: Option<&str>,
) -> Occurrence {
    let root = directory.join("sources");
    fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let source = root.join(name);
    fs::write(&source, b"test").unwrap();
    let hash = media::hash(&source).unwrap();
    let mut media = sample(hash.clone());
    if name.ends_with(".xmp") {
        media.kind = "sidecar".into();
    }
    let occurrence = Occurrence {
            computer: None,
        id: name.into(),
        item: name.into(),
        source: source.to_string_lossy().into(),
        root: root.to_string_lossy().into(),
        relative: name.into(),
        album: "Sources".into(),
        hash,
        identity: media::identity(&source).unwrap(),
        status: "pending".into(),
        bytes: 4,
    };
    let item = Item {
        id: name.into(),
        path: format!("photos/{name}"),
        original_name: name.into(),
        state: "active".into(),
        keeper: None,
        media,
        settings: Settings::default(),
        albums: vec!["Sources".into()],
    };
    if let Some(job) = job {
        archive
            .place_import(&source, item, occurrence.clone(), job, &Default::default())
            .unwrap();
    } else {
        archive
            .place(&source, item, Some(occurrence.clone()), None)
            .unwrap();
    }
    occurrence
}

#[test]
fn clear_import_only_trashes_exclusive_new_copies_and_never_sources() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("archive")).unwrap();
    let mut archive = Archive::open(&directory.path().join("archive")).unwrap();
    archive.save_job("cancelled", &serde_json::json!({"id":"cancelled","tracksCopies":true,"progress":{"status":"queued"}})).unwrap();
    archive
        .save_job(
            "cancelled",
            &serde_json::json!({"progress":{"status":"cancelled"}}),
        )
        .unwrap();
    archive.save_job("other", &serde_json::json!({"id":"other","tracksCopies":true,"progress":{"status":"cancelled"}})).unwrap();
    let removable =
        import_copy_fixture(&mut archive, directory.path(), "new.jpg", Some("cancelled"));
    let shared = import_copy_fixture(
        &mut archive,
        directory.path(),
        "shared.jpg",
        Some("cancelled"),
    );
    let existing = import_copy_fixture(&mut archive, directory.path(), "existing.jpg", None);
    archive
        .record_import_occurrences("cancelled", std::slice::from_ref(&existing))
        .unwrap();
    archive
        .record_import_occurrences("other", std::slice::from_ref(&shared))
        .unwrap();
    archive
        .clear_import_with("other", false, |_| panic!("Keep must not call Trash"))
        .unwrap();
    let related = import_copy_fixture(
        &mut archive,
        directory.path(),
        "pair.jpg",
        Some("cancelled"),
    );
    let sidecar = import_copy_fixture(
        &mut archive,
        directory.path(),
        "pair.xmp",
        Some("cancelled"),
    );
    let unavailable = import_copy_fixture(
        &mut archive,
        directory.path(),
        "changed.jpg",
        Some("cancelled"),
    );
    fs::write(&unavailable.source, b"changed").unwrap();
    let preview = archive.clear_import_preview("cancelled").unwrap();
    assert!(preview.tracks_copies);
    assert_eq!((preview.removable, preview.retained), (1, 4));
    let trash = directory.path().join("fake-trash.jpg");
    let mut calls = 0;
    archive
        .clear_import_with("cancelled", true, |path| {
            calls += 1;
            fs::rename(path, &trash).map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(fs::read(&trash).unwrap(), b"test");
    assert_eq!(archive.item(&removable.item).unwrap().state, "purged");
    assert!(archive.jobs().unwrap().is_empty());
    for source in [&removable, &shared, &existing, &related, &sidecar] {
        assert_eq!(fs::read(&source.source).unwrap(), b"test");
    }
    for source in [&shared, &existing, &related, &sidecar, &unavailable] {
        let item = archive.item(&source.item).unwrap();
        assert_eq!(item.state, "active");
        assert!(archive.root.join(item.path).is_file());
    }
}

#[test]
fn clear_import_keeps_history_and_recoverable_copy_when_trash_fails() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("archive")).unwrap();
    let mut archive = Archive::open(&directory.path().join("archive")).unwrap();
    archive.save_job("cancelled", &serde_json::json!({"id":"cancelled","tracksCopies":true,"progress":{"status":"cancelled"}})).unwrap();
    let source = import_copy_fixture(
        &mut archive,
        directory.path(),
        "photo.jpg",
        Some("cancelled"),
    );
    let error = archive
        .clear_import_with("cancelled", true, |_| Err("Trash unavailable".into()))
        .unwrap_err();
    assert!(error.contains("Trash unavailable"));
    assert_eq!(archive.jobs().unwrap().len(), 1);
    let item = archive.item(&source.item).unwrap();
    assert_eq!(item.state, "undo");
    assert_eq!(fs::read(archive.root.join(&item.path)).unwrap(), b"test");
    assert_eq!(fs::read(&source.source).unwrap(), b"test");
    archive
        .relocate(
            &source.item,
            Path::new(".singulo/undo/interrupted.jpg").into(),
            "trash-pending",
            None,
        )
        .unwrap();
    assert!(!archive.recover().unwrap().is_empty());
    let recovered = archive.item(&source.item).unwrap();
    assert_eq!(recovered.state, "undo");
    assert_eq!(
        fs::read(archive.root.join(recovered.path)).unwrap(),
        b"test"
    );
    fs::write(&source.source, b"fake").unwrap();
    let mut changed_source = source.clone();
    changed_source.identity = media::identity(Path::new(&source.source)).unwrap();
    archive
        .record_import_occurrences("cancelled", &[changed_source])
        .unwrap();
    let error = archive
        .clear_import_with("cancelled", true, |_| {
            panic!("Changed bytes must not reach Trash")
        })
        .unwrap_err();
    assert!(error.contains("Source content no longer matches"));
    assert_eq!(fs::read(&source.source).unwrap(), b"fake");
    let retained = archive.item(&source.item).unwrap();
    assert_eq!(fs::read(archive.root.join(retained.path)).unwrap(), b"test");
}

#[test]
fn clear_import_protects_legacy_files_and_retains_older_incomplete_history() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("archive")).unwrap();
    let mut archive = Archive::open(&directory.path().join("archive")).unwrap();
    let source = import_copy_fixture(&mut archive, directory.path(), "legacy.jpg", None);
    archive
        .save_job(
            "legacy",
            &serde_json::json!({"id":"legacy","progress":{"status":"cancelled"}}),
        )
        .unwrap();
    archive.save_job("retried", &serde_json::json!({"id":"retried","roots":["/retry"],"progress":{"status":"cancelled"}})).unwrap();
    archive.save_job("successful-retry", &serde_json::json!({"id":"successful-retry","roots":["/retry"],"progress":{"status":"complete"}})).unwrap();
    for index in 0..25 {
        archive
            .save_job(
                &format!("complete-{index}"),
                &serde_json::json!({"progress":{"status":"complete"}}),
            )
            .unwrap();
    }
    assert!(
        archive
            .jobs()
            .unwrap()
            .iter()
            .any(|job| job["id"] == "legacy")
    );
    assert!(!archive.jobs().unwrap().iter().any(|job| job["id"] == "retried"));
    archive.save_job("interrupted", &serde_json::json!({"id":"interrupted","roots":["/twice"],"progress":{"status":"cancelled"}})).unwrap();
    archive.save_job("interrupted-retry", &serde_json::json!({"id":"interrupted-retry","roots":["/twice"],"progress":{"status":"importing"}})).unwrap();
    let listed = |archive: &Archive, id: &str| archive.jobs().unwrap().iter().any(|job| job["id"] == id);
    assert!(listed(&archive, "interrupted-retry") && !listed(&archive, "interrupted"));
    archive
        .clear_import_with("interrupted-retry", false, |_| panic!("Keep must not call Trash"))
        .unwrap();
    // The earlier attempt may still own copies, so it returns once the later one is cleared.
    assert!(listed(&archive, "interrupted") && !listed(&archive, "interrupted-retry"));
    assert!(
        !archive
            .clear_import_preview("legacy")
            .unwrap()
            .tracks_copies
    );
    assert!(
        archive
            .clear_import_with("legacy", true, |_| panic!(
                "Legacy files must never be trashed"
            ))
            .is_err()
    );
    archive
        .clear_import_with("legacy", false, |_| panic!("Keep must not call Trash"))
        .unwrap();
    assert_eq!(fs::read(&source.source).unwrap(), b"test");
    assert_eq!(
        fs::read(archive.root.join("photos/legacy.jpg")).unwrap(),
        b"test"
    );
}

#[test]
fn photo_comparison_preserves_meaningful_variants() {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(200, 160, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    }));
    let preview = |image: &DynamicImage| {
        let (w, h) = image.dimensions();
        media::Preview::new(image, w, h)
    };
    assert!(media::photo_equivalent(&preview(&image), &preview(&image)));
    let crop = image.crop_imm(10, 8, 180, 144);
    assert!(!media::photo_equivalent(&preview(&image), &preview(&crop)));
    // Mild crop/edit still qualifies for Similarity Review
    let brightened = DynamicImage::ImageRgb8(ImageBuffer::from_fn(200, 160, |x, y| {
        Rgb([
            ((x % 256) as u8).saturating_add(12),
            ((y % 256) as u8).saturating_add(12),
            (((x + y) % 256) as u8).saturating_add(12),
        ])
    }));
    assert!(media::visually_similar(&preview(&image), &preview(&brightened)));

    // Two unrelated mostly-white UI screenshots (landscape cart vs portrait file list)
    // must never be flagged as visually similar for review.
    let cart = DynamicImage::ImageRgb8(ImageBuffer::from_fn(831, 398, |x, y| {
        if !(35..355).contains(&y) {
            Rgb([24, 42, 35])
        } else if (48..72).contains(&x) && (y % 75) < 28 {
            Rgb([95, 70, 55])
        } else {
            Rgb([255, 255, 255])
        }
    }));
    let file_list = DynamicImage::ImageRgb8(ImageBuffer::from_fn(296, 500, |x, y| {
        if (28..48).contains(&x) && (y % 38) < 22 {
            Rgb([60, 130, 180])
        } else if (56..170).contains(&x) && (y % 38).abs_diff(12) < 4 {
            Rgb([40, 40, 40])
        } else {
            Rgb([255, 255, 255])
        }
    }));
    assert!(!media::visually_similar(&preview(&cart), &preview(&file_list)));
    assert!(!media::aspect_compatible(831, 398, 296, 500));
}

#[test]
fn scaled_jpeg_matches_full_decode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.jpg");
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(2400, 1600, |x, y| {
        Rgb([
            (x / 10) as u8,
            (y / 7) as u8,
            ((x / 20 + y / 20) % 256) as u8,
        ])
    }));
    image.save(&path).unwrap();
    let (scaled, width, height) = media::scaled_jpeg(&path).unwrap().unwrap();
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    let (buffered, buffered_width, buffered_height) =
        media::scaled_jpeg_reader(std::io::Cursor::new(&bytes), 512)
            .unwrap()
            .unwrap();
    assert_eq!((buffered_width, buffered_height), (width, height));
    assert_eq!(buffered.to_rgb8(), scaled.to_rgb8());
    assert_eq!((width, height), (2400, 1600));
    assert!(scaled.width() < 2400 && scaled.height() >= 512);
    let full = image::load_from_memory(&bytes).unwrap();
    assert!(media::photo_equivalent(
        &media::Preview::new(&scaled, width, height),
        &media::Preview::new(&full, 2400, 1600)
    ));
}

#[test]
fn video_review_candidates_require_capture_day_and_close_duration() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mov");
    let target = dir.path().join("archive");
    fs::create_dir(&target).unwrap();
    fs::write(&source, b"test").unwrap();
    let hash = media::hash(&source).unwrap();
    let mut archive = Archive::open(&target).unwrap();
    let legacy_settings: Settings = serde_json::from_value(serde_json::json!({
        "template": "{yyyy}/{mm}/{dd}", "filenames": "original",
        "unknownFolder": "unknown", "automaticMatches": true
    }))
    .unwrap();
    assert!(!legacy_settings.deep_video_matching);
    assert!(!legacy_settings.prevent_sleep_during_import);
    for (id, day, duration, capture) in [
        ("same", "2023-07-18", Some(100.0), true),
        ("boundary", "2023-07-18", Some(101.0), true),
        ("longer", "2023-07-18", Some(101.1), true),
        ("other-day", "2023-07-19", Some(100.0), true),
        ("unknown-duration", "2023-07-18", None, true),
        ("estimated", "2023-07-18", Some(100.0), false),
        ("undo", "2023-07-18", Some(100.0), true),
    ] {
        let mut media = sample(hash.clone());
        media.kind = "video".into();
        media.date = Some(format!("{day}T12:00:00"));
        if !capture {
            media.date_source = "estimated: file modification".into();
        }
        media.duration_seconds = duration;
        archive
            .place(
                &source,
                Item {
                    id: id.into(),
                    path: format!("{id}.mov"),
                    original_name: format!("{id}.mov"),
                    state: if id == "undo" { "undo" } else { "active" }.into(),
                    keeper: None,
                    media,
                    settings: Settings::default(),
                    albums: vec![],
                },
                None,
                None,
            )
            .unwrap();
    }
    let mut incoming = sample(hash);
    incoming.kind = "video".into();
    incoming.duration_seconds = Some(100.0);
    let matches = archive.video_candidates(&incoming).unwrap();
    assert_eq!(
        matches
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["same", "boundary"]
    );
    incoming.duration_seconds = None;
    assert!(archive.video_candidates(&incoming).unwrap().is_empty());
    incoming.duration_seconds = Some(100.0);
    incoming.date_source = "estimated: file modification".into();
    assert!(archive.video_candidates(&incoming).unwrap().is_empty());
}

#[test]
fn reorganize_and_date_correction_handle_collisions_and_preserve_time() {
    let dir = tempfile::tempdir().unwrap();
    let s1 = dir.path().join("IMG_1234.CR2");
    let s2 = dir.path().join("other_IMG_1234.CR2");
    let xmp = dir.path().join("IMG_1234.CR2.xmp");
    let target = dir.path().join("archive");
    fs::create_dir(&target).unwrap();
    fs::write(&s1, b"photo-one").unwrap();
    fs::write(&s2, b"photo-two").unwrap();
    fs::write(&xmp, b"<xmp/>").unwrap();
    let mut archive = Archive::open(&target).unwrap();

    let h1 = media::hash(&s1).unwrap();
    let h2 = media::hash(&s2).unwrap();
    let hx = media::hash(&xmp).unwrap();
    let first = Item {
        id: "id-1".into(),
        path: "2023/07/18/IMG_1234.CR2".into(),
        original_name: "IMG_1234.CR2".into(),
        state: "active".into(),
        keeper: None,
        media: Media {
            kind: "raw".into(),
            ..sample(h1.clone())
        },
        settings: Settings::default(),
        albums: vec![],
    };
    let second = Item {
        id: "id-2".into(),
        path: "2023/07/18/IMG_1234__id-2.CR2".into(),
        original_name: "IMG_1234.CR2".into(),
        state: "active".into(),
        keeper: None,
        media: Media {
            kind: "raw".into(),
            ..sample(h2)
        },
        settings: Settings::default(),
        albums: vec![],
    };
    let sidecar = Item {
        id: "id-xmp".into(),
        path: "2023/07/18/IMG_1234.xmp".into(),
        original_name: "IMG_1234.CR2.xmp".into(),
        state: "active".into(),
        keeper: None,
        media: Media {
            kind: "sidecar".into(),
            ..sample(hx.clone())
        },
        settings: Settings::default(),
        albums: vec![],
    };
    let root_str = dir.path().canonicalize().unwrap().to_string_lossy().into_owned();
    archive
        .place(
            &s1,
            first,
            Some(Occurrence {
            computer: None,
                id: "occ-1".into(),
                item: "id-1".into(),
                source: s1.canonicalize().unwrap().to_string_lossy().into(),
                root: root_str.clone(),
                album: "Card".into(),
                relative: "IMG_1234.CR2".into(),
                hash: h1,
                identity: media::identity(&s1).unwrap(),
                status: "pending".into(),
                bytes: 9,
            }),
            None,
        )
        .unwrap();
    archive.place(&s2, second, None, None).unwrap();
    archive
        .place(
            &xmp,
            sidecar,
            Some(Occurrence {
            computer: None,
                id: "occ-xmp".into(),
                item: "id-xmp".into(),
                source: xmp.canonicalize().unwrap().to_string_lossy().into(),
                root: root_str,
                album: "Card".into(),
                relative: "IMG_1234.CR2.xmp".into(),
                hash: hx,
                identity: media::identity(&xmp).unwrap(),
                status: "pending".into(),
                bytes: 6,
            }),
            None,
        )
        .unwrap();

    // .CR2.xmp links to .CR2 companion
    assert!(archive.companions("id-1").unwrap().contains(&"id-xmp".to_string()));
    // Reorganize preview with existing __<id> collision is a no-op rather than failing
    assert!(archive.move_preview(&[]).unwrap().is_empty());

    // Correcting date on same day preserves HH:MM:SS and does not add a self-collision suffix
    archive.correct_date("id-1", "2023-07-18").unwrap();
    let updated = archive.item("id-1").unwrap();
    assert_eq!(updated.path, "2023/07/18/IMG_1234.CR2");
    assert_eq!(updated.media.date.as_deref(), Some("2023-07-18T14:32:10"));

    // Correcting to a new day moves both photo and sidecar while keeping time-of-day
    archive.correct_date("id-1", "2023-07-19").unwrap();
    assert_eq!(archive.item("id-1").unwrap().path, "2023/07/19/IMG_1234.CR2");
    assert_eq!(archive.item("id-xmp").unwrap().path, "2023/07/19/IMG_1234.xmp");
    assert_eq!(
        archive.item("id-1").unwrap().media.date.as_deref(),
        Some("2023-07-19T14:32:10")
    );

    // Now id-2 can cleanly take the vacated unsuffixed name on reorganization
    let moves = archive.move_preview(&[]).unwrap();
    assert_eq!(moves.len(), 1);
    assert_eq!(moves[0].to, "2023/07/18/IMG_1234.CR2");
    archive.reorganize(&moves).unwrap();
    assert!(archive.move_preview(&[]).unwrap().is_empty());
}
