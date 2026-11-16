# Photo Archive

A small, offline desktop app for organizing pictures and videos. Built with Tauri 2, Rust and Svelte.

The interface is available in English and Dutch. Choose a language on the Settings page; your choice is saved on this computer.

## Use it

1. Choose a dedicated archive folder, usually on an external disk.
2. Drop source folders onto the main app or its floating, always-on-top drop window.
3. Browse the library or separate viewer. Search original folder names such as **Holiday Germany**; folder history survives deduplication.
4. Compare uncertain matches in **Similarity review**. The overview shows thumbnails of both files. Use **Review next match** to work through them one by one; after keeping or dismissing a match, the next one opens automatically. You can opt out of the keeper confirmation there. Keep both when they are different edits, crops, burst frames or RAW/JPEG companions.

Keeping one copy moves the other to archive undo. A picture with related RAW/JPEG, sidecar or Live Photo files cannot be moved to undo on its own. If only one picture has related files, you can keep it and move the other to undo. If both have related files, choose **Keep both / dismiss match**; both remain active and the review is cleared.

In comparisons, a file size or pixel count at least twice the other is highlighted. A **Probably better** label appears only when the other available measurement does not favor the other picture. These are hints, not guarantees of image quality; inspect both before deciding.

The library and separate viewer let you choose a year and month (or unknown dates) and show up to 60 thumbnails per page. Search and filters apply across the archive; the year and month choices show matching photos. The separate viewer is browsing-only, without import or settings controls. Click a thumbnail for a large photo, use Previous/Next or the arrow keys to browse across pages within the selected month, and Escape to return to thumbnails. Videos have a Play video button.

Video playback tries the archived original first. If the built-in player cannot play it, the app prepares and caches a playable copy in the background; the original is never changed. Playback support depends on the system and the video's container and codecs.

Imports **copy**, never delete or modify sources. Exact duplicates share one verified archived copy. Conservative visual matching can keep a clearly higher-resolution equivalent active; other original bytes remain in **archive undo** until you explicitly purge them. Resolution and visual comparisons are quality estimates, not a promise of photographic quality.

### Dates and folder layout

Default: `yyyy/mm/dd/original-name.ext`.

Settings offer date-layout presets, original or date/time filenames, an unknown-date folder, and a restricted advanced template:

- `{yyyy}`, `{mm}`, `{dd}`, `{media_type}`
- Example: `Family/{yyyy}/{mm}`.
- Collisions get a unique suffix; unrelated files are never overwritten.

Capture metadata comes first, then filesystem creation time, then **visibly estimated** modification time. Unusable/default dates go to `unknown`. No timezone means the recorded calendar date is retained, with a warning for ambiguous video timestamps. Correct dates from the viewer.

Settings belong to each archive and affect new import jobs. **Preview reorganization** applies saved settings to existing managed files. A reversible recovery manifest is saved; **Preview undo of last reorganization** restores the previous layout when there are no conflicts.

### Source cleanup

**Review source cleanup** is a separate, explicitly confirmed operation:

- Re-reads source and archive hashes. Only unchanged sources with a byte-identical archived original qualify.
- A similar-looking keeper alone is not enough; the exact original must still exist in the archive or archive undo.
- Moves sources to native Trash/Recycle Bin. Unsupported/failed recycling never falls back to permanent deletion.
- Optionally removes genuinely empty imported subfolders, never dropped roots or nonempty folders.
- A temporary folder beside the source isolates the verified file during cleanup. If recovery fails, the app reports its location; do not delete it blindly.

**Trash usually does not free disk space until you empty it yourself.** The app does not empty system trash. Restore source files through the OS, not archive undo.

After cleanup the archive may be your only copy. Keep another backup, especially before purging archive undo. A hash verifies a copy; it does not make a single external disk a safe backup strategy.

### Interruptions and recovery

Pause/cancel imports from the overview. Retry saved folders after a failure; exact copies are recognized again. Disk disconnection is reported, never replaced by a new archive elsewhere.

The archive contains ordinary media files and `.photo-dedupe/` with the SQLite catalog, staging/undo/cache folders, job history, catalog backup and provenance export. Move the whole archive between computers, including this folder. Never edit the catalog while the app is running.

Interrupted copy/relocation jobs are reconciled on opening. Conflicting/incomplete output files are reported with staging retained rather than overwritten. Stop the app and preserve the archive before manually resolving a conflict or restoring `recovery/catalog-latest.sqlite`. JSON metadata exports retain source-folder history.

## Supported media

| Family | Import | Preview / similarity |
| --- | --- | --- |
| JPEG, PNG, WebP, TIFF, BMP, GIF | Yes | Rust image decoder; animated files need review |
| HEIC/HEIF, AVIF | Yes | Bundled libheif, subject to decoder support |
| Camera RAW, including DNG/CR2/CR3/NEF/ARW/RAF | Yes | LibRaw supported-camera list; originals are preserved |
| MP4/MOV, AVI/MKV/WebM and other recognized videos | Yes | Direct playback where supported; otherwise a cached FFmpeg proxy |
| XMP sidecars | Yes | Preserved and associated where identifiable |

Unsupported/corrupt previews are explicitly marked; recognized originals can still be copied and exact-deduplicated. RAW/JPEG and identifiable Live Photo companions are protected from automatic replacement.

Videos use exact SHA-256 deduplication by default. Possible matches from a preview fingerprint or the same capture day and a close duration go to manual review; neither original is automatically replaced. Duration matching allows up to 1 second or 1%, whichever is larger, and ignores estimated filesystem dates. Older catalog entries without stored durations still participate through their fingerprints. At most ten candidates are considered per incoming video.

Settings offers **Deep video comparison (slower)**, off by default, including for existing archives. Together with automatic grouping, this enables full-timeline checks of frame counts, stream properties and audio before a clearly higher-resolution equivalent can replace a smaller one. Changed audio, trimmed clips, extra streams, HDR/format differences and uncertain quality cannot qualify automatically. These checks can substantially slow imports.

Targets: Windows 10/11 x64 and macOS 12+ Intel/Apple Silicon. Only combinations actually smoke-tested should be advertised as verified. Do not assume every camera, codec, removable filesystem or network share is supported; one writer per archive, local disks only.

## Development

Prerequisites: Node (the version required by the current Vite release), Rust and the platform's Tauri prerequisites. On macOS install Xcode command-line tools.

```sh
npm install
npm run desktop
```

Debug builds discover `exiftool`, `ffmpeg`, `ffprobe`, `dcraw_emu`, and `heif-convert` on `PATH` (or standard Homebrew paths on macOS). **Media tools** in settings reports availability. Development tools may have different licenses; do not redistribute them without checking.

```sh
npm run check
npm run build
npm test
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
```

Focused Rust tests and JavaScript comparison-rule tests are included, but they are not an extensive framework. Manually smoke-test import/re-import, matching, viewing, layout changes/undo, recycling on disposable data, and an interrupted operation.

### Large collections

The catalog is designed for 100,000+ records: source/companion lookups are indexed, library pages are fetched in SQL, counts are maintained incrementally, and recovery exports are streamed rather than loading all metadata into memory. Folder-name collision checks cache each destination directory once per import instead of rescanning it for every photo.

Dropping multiple folders together creates one import job. All folders are scanned first, then files are processed in scan order, with XMP sidecars last. Further drops queue behind the current job rather than competing for the disk.

Metadata extraction batches up to 64 files per ExifTool process. Exact duplicates are checked before image decoding. Verified staging files are linked into their final location on supporting filesystems (APFS/NTFS); filesystems without hard links retain the verified-copy fallback. Duplicate source-history writes are committed together per batch, and related review entries per incoming file, without relaxing durable writes. If a duplicate-history batch fails, it rolls back and reports errors rather than claiming successful deduplication. UI progress updates are limited to four per second, and the app crate plus image/hash dependencies are optimized even in development builds.

Expand **Import timings** on the overview to see scanning, hashing/exact verification, metadata, input buffering, decoding, matching, placement/catalog writes, recovery backup, and total elapsed time. Completed timings are saved with the job. Decoder time is summed across workers and overlaps other stages, so stage totals are not wall-clock throughput benchmarks. Placement includes durable flushing and any extra staging-to-final copy.

#### How similarity matching works

1. **Exact:** SHA-256 of the file bytes; byte-identical backups never get decoded.
2. **Fingerprint:** a 64-bit difference hash (dHash) of a small greyscale version. SQLite indexes its 8 bytes, so candidates (≤ 6 differing bits) are found in about a millisecond at 100k photos.
3. **Pixel check:** before a larger copy replaces a smaller one, both are compared on a 512×512 grid, tile by tile. Protected variants (RAW/JPEG pairs, edits, Live Photos, different capture times) and anything uncertain go to Similarity review instead.

Each new file is decoded **once** for import, at reduced size, and that preview is reused for both the fingerprint and the pixel check. JPEGs are decoded with DCT scaling (1/2–1/8 size, short side never below 512 px), and RAW uses LibRaw half-size mode. Photos up to 16 MiB are read into bounded memory buffers on the coordinator, then decoded on up to 8 CPU-only worker threads (CPU cores - 1). Queued/in-flight compressed inputs have a 64 MiB budget. Larger photos and external RAW/HEIC/video decoders stay on the coordinator, alongside hashing, metadata extraction, candidate reads and archive writes; image workers do not reopen source files. This avoids concurrent source reads, although batching and candidate lookups still involve seeks. Results are committed in the original order, so sidecars still follow their photos.

Comparison previews use a 128-entry least-recently-used cache rather than discarding the whole cache when full. Cached pixels are shared instead of cloned for each comparison. This cache lasts for the import job; archived bytes still require verification before automatic deduplication. Browsing thumbnails are cached separately and can require another decode.

Vision models such as CLIP or DINOv2 are deliberately not used: they still need the decoded image, add hundreds of MB, and group *similar scenes* (bursts, re-shoots, crops), which is useful for browsing but too loose to decide what replaces what.

These changes remove repeated whole-library work, not the necessary cost of reading originals, decoding new photos, verifying copies, or comparing an entire video. External HDD/USB speed and RAW/video content still determine throughput. Pause/cancel before restarting the app to pick up an update; the first open of an existing catalog builds its new indexes.

For a Mac-only external archive, prefer **APFS**; for Windows-only, **NTFS**. **exFAT** is convenient when using the disk on both systems, but has no journaling or hard links: current placement reads verified staging and writes a second copy to the final path. Always eject safely and keep another backup. There is no need to reformat an existing disk; formatting erases its files, and this app never formats disks.

An optional synthetic 100k-catalog check is separate from the basic tests:

```sh
cargo test --manifest-path src-tauri/Cargo.toml catalog_handles_100k_records -- --ignored --nocapture
```

On the development Mac, this check measured a deep library page at approximately 16 ms, an indexed companion link at 3 ms, and a streamed backup at 0.6 seconds. These are catalog timings on synthetic records, **not** a promise of 100k-photo import speed or performance on an external disk. A separate 16-file metadata check measured about 1.67 seconds with individual processes versus 0.12 seconds batched.

### App icon

The original stacked-photo artwork is in [static/app-icon.svg](static/app-icon.svg). Native Windows/macOS icons in [src-tauri/icons](src-tauri/icons) and the PNG favicon are generated from it with Tauri's icon command.

```sh
npm run tauri -- icon static/app-icon.svg --output /path/to/temporary-icons
```

Copy the generated `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.png`, `icon.icns` and `icon.ico` into `src-tauri/icons`, and `32x32.png` to `static/favicon.png`. Mobile and Windows Store artwork is not needed for the current desktop installers.

## Offline distribution

Release builds require a complete architecture-matching media bundle; they **never** silently depend on Homebrew or an end-user command-line installation.

Prepare a directory with these executables and their libraries/runtimes:

```text
media-tools/
  ffmpeg[.exe]
  ffprobe[.exe]
  dcraw_emu[.exe]
  heif-convert[.exe]
  exiftool[.exe]
  lib/                    # ExifTool Perl modules on macOS
  exiftool_files/          # Windows ExifTool runtime, when required
  licenses/               # Notices, corresponding sources, build instructions
```

Use FFmpeg without `--enable-gpl` or `--enable-nonfree`; enable the H.264 preview encoder (`h264_videotoolbox` on macOS, `libopenh264` on Windows). Build libheif without GPL encoder dependencies. Include required notices and corresponding sources for LibRaw/libheif/FFmpeg and transitive dependencies. Codec patent considerations are separate from open-source licenses.

```sh
npm run prepare:media -- /absolute/path/to/media-tools
npm run tauri -- build
```

Preparation verifies required tools and FFmpeg configuration, relocates non-system macOS libraries, rejects known GPL encoder libraries, copies license material, and generates a bundle manifest. Windows inputs must already include all DLLs/runtime files. Smoke-test packaged apps on clean targets; preparation is not a substitute for that.

Signing/notarization requires your own certificates/accounts. Unsigned local builds are not notarized. Bundled codecs increase package size; measure the actual installer.
