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
    assert_eq!(archive.page("old backup", "all", 0).unwrap().len(), 1);
    let mut occurrence = archive.item_occurrences("original").unwrap().remove(0);
    occurrence.album = "Été".into();
    archive.record_occurrence(&occurrence).unwrap();
    assert_eq!(archive.page("été", "all", 0).unwrap().len(), 1);
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
    fs::write(target.join("IMG.jpg"), b"fake").unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
    fs::write(&source, b"changed").unwrap();
    assert!(archive.cleanup_check(&occurrence).is_err());
}

#[test]
fn photo_comparison_preserves_meaningful_variants() {
    let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(128, 128, |x, y| {
        Rgb([(x * 2) as u8, (y * 2) as u8, ((x + y) % 256) as u8])
    }));
    let preview = |image: &DynamicImage| {
        let (w, h) = image.dimensions();
        media::Preview::new(image, w, h)
    };
    assert!(media::photo_equivalent(&preview(&image), &preview(&image)));
    let crop = image.crop_imm(10, 10, 100, 100);
    assert!(!media::photo_equivalent(&preview(&image), &preview(&crop)));
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
    assert_eq!((width, height), (2400, 1600));
    assert!(scaled.width() < 2400 && scaled.height() >= 512);
    let full = image::open(&path).unwrap();
    assert!(media::photo_equivalent(
        &media::Preview::new(&scaled, width, height),
        &media::Preview::new(&full, 2400, 1600)
    ));
}
