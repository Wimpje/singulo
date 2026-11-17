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
    fs::write(&source, b"test").unwrap();
    let source = source.canonicalize().unwrap();
    let hash = media::hash(&source).unwrap();
    let mut archive = Archive::open(&target).unwrap();
    let item = Item {
        id: "original".into(),
        path: "IMG.jpg".into(),
        original_name: "IMG.jpg".into(),
        state: "active".into(),
        keeper: None,
        media: sample(hash.clone()),
        settings: Settings::default(),
        albums: vec![],
    };
    archive.place(&source, item, None, None).unwrap();
    let occurrence = Occurrence {
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
        bytes: 4,
    };
    assert!(archive.cleanup_check(&occurrence).is_ok());
    archive.record_occurrence(&occurrence).unwrap();
    let reader = archive.cleanup_reader().unwrap();
    assert!(reader.cleanup_rows().unwrap()[0].eligible);
    fs::write(target.join("IMG.jpg"), b"fake").unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
    assert!(!reader.cleanup_rows().unwrap()[0].eligible);
    fs::write(&source, b"changed").unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
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
            .place_import(&source, item, occurrence.clone(), job)
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
            Path::new(".photo-dedupe/undo/interrupted.jpg").into(),
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
