# Singulo

A small, offline desktop app for organizing pictures and videos. Built with Tauri 2, Rust and Svelte.

Made by **nosmallthing**.

The interface is available in English and Dutch. Choose a language on the Settings page; your choice is saved on this computer.

Note that this is an AI coded application, created for my personal use. I hope someone will like it :-) 

## Use it

1. Choose a dedicated archive folder, usually on an external disk.
2. Click **Add folders** in the sidebar, or drop source folders anywhere on the main window or its floating, always-on-top drop window. Import progress stays visible in the sidebar.
3. Browse **Photos**, the app's start page, or the separate viewer. Search original folder names such as **Holiday Germany**; folder history survives deduplication.
4. Compare uncertain matches in **Similarity review**. The overview shows thumbnails of both files. **Start reviewing** opens them one by one with a match counter; after keeping or dismissing a match, the next one opens automatically. Keyboard: `1` / `2` keep the left / right copy, `B` keeps both, `S` or `→` skips, `←` goes back, `Z` zooms both pictures, `Esc` closes. Click a picture to zoom both at the same spot. Keep both when they are different edits, crops, burst frames or RAW/JPEG companions.

Keeping one copy removes the other from the library, without a confirmation dialog. It is not deleted: it becomes a **spare copy of a removed duplicate** (Dutch: *reservekopie van een verwijderde dubbele foto*), stored in archive undo (`.singulo/undo`). A message offers **Undo** (or Cmd/Ctrl+Z) for a few seconds, which restores the copy and puts the match back in review. Spare copies stay restorable later through the **Spare copies of removed duplicates** filter in Photos, until you delete them permanently. A picture with related RAW/JPEG, sidecar or Live Photo files cannot be moved to undo on its own. If only one picture has related files, you can keep it and move the other to undo. If both have related files, choose **Keep both / dismiss match**; both remain active and the review is cleared.

In comparisons, a table lists resolution, file size, type, date, original folders, related files and location, with differing rows highlighted. A file size or pixel count at least twice the other is highlighted. A **Probably better** label appears only when the other available measurement does not favor the other picture. These are hints, not guarantees of image quality; inspect both before deciding.

The library and separate viewer show a timeline of years and months (or unknown dates) next to the photo grid; more photos load as you scroll. Use **Newer** / **Older** to step between months, and the size slider to change thumbnail size. Search and filters apply across the archive, and the timeline shows matching photos. The separate viewer is browsing-only, without import, settings or editing controls. Click a thumbnail to open it full-window; use the arrows or arrow keys to browse, continuing into the next or previous month, `I` to toggle the info panel, and Escape to return to the same spot in the grid. The info panel shows date and its source, dimensions, original folders and a **Show in Finder / Explorer** button. Videos have a Play video button.

Video playback tries the archived original first. If the built-in player cannot play it, the app prepares and caches a playable copy in the background; the original is never changed. Playback support depends on the system and the video's container and codecs.

Imports **copy**, never delete or modify sources. Exact duplicates share one verified archived copy. Conservative visual matching can keep a clearly higher-resolution equivalent active; other original bytes remain in **archive undo** until you explicitly purge them. Resolution and visual comparisons are quality estimates, not a promise of photographic quality.

### Dates and folder layout

Default: `yyyy/mm/dd/original-name.ext`.

Settings offer date-layout presets, original or date/time filenames, an unknown-date folder, and a restricted advanced template:

- `{yyyy}`, `{mm}`, `{dd}`, `{media_type}`
- Example: `Family/{yyyy}/{mm}`.
- Collisions get a unique suffix; unrelated files are never overwritten.

Capture metadata comes first, then filesystem creation time, then **visibly estimated** modification time. If none provides a usable date, a valid date in the selected source folder path (for example `2024/07/18`) is used as an estimate. Date folders are omitted from original-folder labels; other folders are searchable both as a full path and as individual labels. If no file or folder date is usable, the item goes to `unknown`. No timezone means the recorded calendar date is retained, with a warning for ambiguous video timestamps. Correct dates from the viewer.

Settings belong to each archive and affect new import jobs. **Preview reorganization** applies saved settings to existing managed files. A reversible recovery manifest is saved; **Preview undo of last reorganization** restores the previous layout when there are no conflicts.

### Source cleanup

**Review source cleanup** is a separate, explicitly confirmed operation:

- The preview opens immediately while source/archive verification runs. Choose whole source folders by default, or expand a folder to select individual files and see why files are being kept. File details load in small pages.
- Re-reads source and archive hashes. Only unchanged sources with a byte-identical archived original qualify.
- A similar-looking keeper alone is not enough; the exact original must still exist in the archive or archive undo.
- Moves sources to native Trash/Recycle Bin. Unsupported/failed recycling never falls back to permanent deletion.
- Optionally removes genuinely empty imported subfolders, never dropped roots or nonempty folders.
- A temporary folder beside the source isolates the verified file during cleanup. If recovery fails, the app reports its location; do not delete it blindly.

**Trash usually does not free disk space until you empty it yourself.** The app does not empty system trash. Restore source files through the OS, not archive undo.

After cleanup the archive may be your only copy. Keep another backup, especially before purging archive undo. A hash verifies a copy; it does not make a single external disk a safe backup strategy.

### Interruptions and recovery

Pause/cancel imports on the **Import & archive** page (the sidebar progress links there). Cancel stops the active import and cancels queued imports. On Windows, Singulo asks Windows to interrupt pending reads, including reads of cloud-only OneDrive files. The app shows **Cancelling import…** while it waits for the current operation to stop safely. Verified copies already saved to the archive are kept, and source files are never changed. Retry saved folders after a failure; exact copies are recognized again. Disk disconnection is reported, never replaced by a new archive elsewhere.

While an import is active, a status panel explains why comparisons, source cleanup and archive changes are temporarily unavailable: they could conflict with files still being added or matched. Disabled controls include an explanation on hover. You can still browse photos, switch tabs and queue more folders. Pausing an import keeps these restrictions in place; resume it to finish, or cancel it. Scanning shows activity without a misleading percentage.

**Imports needing attention** keeps canceled, failed, interrupted and partially successful imports visible, including imports older than the recent-history list. **Retry folders** starts the import again or queues it behind another import. New imports record per-folder scan completion, discovered/processed files, copied files, exact duplicates and failures. Expand the issues to see the affected paths and reasons. A retry of the same folders replaces the earlier entry, so repeated interrupted attempts appear once; a successful retry resolves the warning, and verified exact copies are skipped. Clearing the latest attempt shows the earlier one again, because it may still have its own copies. Older imports without folder totals are explicitly marked as needing a retry to establish completeness.

**Clear import** dismisses an incomplete import from history. Keep its already copied archive files (the default), or explicitly choose to move eligible newly created archive copies to native system Trash. Source folders are never changed. Existing/shared copies, related RAW/JPEG/sidecar/Live Photo files, keepers of replaced pictures, and copies without an unchanged available source remain protected. Sources and archive bytes are re-verified before trashing; Trash failure stops clearing and keeps the import visible, with retained staged copies in archive spare copies. Interrupted clearing never resumes trashing automatically. Recover successfully trashed archive copies through system Trash, not archive Undo.

Copy ownership tracking starts with this version. For an older import, the app cannot safely prove which archive files it originally created, so only the keep-files clearing option is available. Clearing history does not erase the safety tracking used to protect copies shared with another import.

The archive contains ordinary media files and `.singulo/` with the SQLite catalog, staging/undo/cache folders, job history, catalog backup and provenance export. Move the whole archive between computers, including this folder. Never edit the catalog while the app is running.

Interrupted copy/relocation jobs are reconciled on opening. Conflicting/incomplete output files are reported with staging retained rather than overwritten. Stop the app and preserve the archive before manually resolving a conflict or restoring `recovery/catalog-latest.sqlite`. JSON metadata exports retain source-folder history.

### Sharing metadata with other Photo organizing tools like Immich

Settings offers **Export XMP** for sharing manually corrected dates and original folder names as tags. Singulo creates marked `filename.ext.xmp` sidecars beside active files and keeps them updated as the archive changes. Original-folder tags include the full non-date folder path and its individual folder names. Originals are never modified. Existing fallback sidecars are merged into a separate export when safe; an occupied export filename, an ambiguous or unsupported sidecar, or an externally modified export is left untouched and reported.

**Remove exported XMP** disables the export and removes only sidecars whose ownership and unchanged contents can be verified. Existing sidecars and modified exports are kept. If an export or removal is interrupted, the app reports pending work on reopening; use Export XMP or Remove exported XMP to resume it. A date without a time remains date-only; photo tools may not apply it, so check reported items. Folder names become tags, not photo albums.

For an Immich external library, mount the archive read-only and exclude `**/.singulo/**` to avoid scanning staging, undo copies and cached previews. Finish any planned reorganization first: Immich can treat a moved external file as a new asset and lose its Immich-side edits. Immich also warns that metadata edits on read-only external libraries may silently fail. See the [Immich external library](https://docs.immich.app/features/libraries/) and [XMP sidecar](https://docs.immich.app/features/xmp-sidecars/) documentation.

## Supported media

| Family | Import | Preview / similarity |
| --- | --- | --- |
| JPEG, PNG, WebP, TIFF, BMP, GIF | Yes | Rust image decoder; animated files need review |
| HEIC/HEIF, AVIF | Yes | Bundled libheif, subject to decoder support |
| Camera RAW, including DNG/CR2/CR3/NEF/ARW/RAF | Yes | LibRaw supported-camera list; originals are preserved |
| MP4/MOV, AVI/MKV/WebM and other recognized videos | Yes | Direct playback where supported; otherwise a cached FFmpeg proxy |
| XMP sidecars | Yes | Preserved and associated where identifiable |

Unsupported/corrupt previews are explicitly marked; recognized originals can still be copied and exact-deduplicated. RAW/JPEG and identifiable Live Photo companions are protected from automatic replacement.

Videos use exact BLAKE3 deduplication by default. Possible matches from a preview fingerprint or the same capture day and a close duration go to manual review; neither original is automatically replaced. Duration matching allows up to 1 second or 1%, whichever is larger, and ignores estimated filesystem dates. Older catalog entries without stored durations still participate through their fingerprints. At most ten candidates are considered per incoming video.

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

The import page shows source folders and queued folders immediately, including while scanning. Live status distinguishes scanning, reading/hashing originals, metadata extraction, RAW/preview decoding, comparing/saving, and final sidecar/recovery work. The stage message and current file update together at most once per second during normal processing; the progress counter and pause or cancel status update immediately. Before the first files are processed, an animated indicator replaces a misleading 0% bar; discovered/checked counts and the current path explain the preparation. Cloud-only originals can take time to download before hashing or decoding can finish. Queued folders are not scanned until their job starts.

The first batch contains at most 4 files to show initial results promptly; subsequent metadata extraction batches contain up to 64 files per ExifTool process. Exact duplicates are checked before image decoding. Verified staging files are linked into their final location on supporting filesystems (APFS/NTFS); filesystems without hard links retain the verified-copy fallback. Duplicate source-history writes are committed together per batch, and related review entries per incoming file, without relaxing durable writes. If a duplicate-history batch fails, it rolls back and reports errors rather than claiming successful deduplication. UI counters update at most four times per second, with immediate updates for job/status/stage changes. The photo library and separate viewer refresh at most once per second as newly copied files become safe to browse, without waiting for the import to finish. The app crate plus image/hash dependencies are optimized even in development builds.

Expand **Import timings** on the **Import & archive** page to see scanning, hashing/exact verification, metadata, input buffering, decoding, matching, placement/catalog writes, recovery backup, and total elapsed time. Completed timings are saved with the job. Decoder time is summed across workers and overlaps other stages, so stage totals are not wall-clock throughput benchmarks. Placement includes durable flushing and any extra staging-to-final copy.

#### How similarity matching works

1. **Exact:** 256-bit BLAKE3 of the file bytes; byte-identical backups never get decoded.
2. **Fingerprint:** a 64-bit difference hash (dHash) of a small greyscale version. SQLite indexes its 8 bytes, so candidates (≤ 6 differing bits) are found in about a millisecond at 100k photos.
3. **Pixel check:** before a larger copy replaces a smaller one, both are compared on a 512×512 grid, tile by tile. Protected variants (RAW/JPEG pairs, edits, Live Photos, different capture times) and anything uncertain go to Similarity review instead.

BLAKE3 also verifies streaming copies, cleanup eligibility, recovery operations, and managed XMP sidecars. Hashing stays on the I/O coordinator with sequential reads; it does not start parallel source-file reads. FFmpeg's optional deep audio comparison and bundled-tool checksums still use SHA-256 independently of archive file hashes.

Each new file is decoded **once** for import, at reduced size, and that preview is reused for both the fingerprint and the pixel check. JPEGs are decoded with DCT scaling (1/2–1/8 size, short side never below 512 px), and RAW (including PEF) uses LibRaw half-size mode. RAW decoder output is decoded directly from memory, avoiding a redundant temporary TIFF write/read without changing pixels or similarity rules. Photos up to 16 MiB are read into bounded memory buffers on the coordinator, then decoded on up to 8 CPU-only worker threads (CPU cores - 1). Queued/in-flight compressed inputs have a 64 MiB budget. Larger photos and external RAW/HEIC/video decoders stay on the coordinator, alongside hashing, metadata extraction, candidate reads and archive writes; image workers do not reopen source files. This avoids concurrent source reads, although batching and candidate lookups still involve seeks. Results are committed in the original order, so sidecars still follow their photos.

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

The media tools are external programs the app starts for metadata, RAW/HEIC decoding, and video inspection/preview. They are **not** needed to compile the Rust or Svelte code. In a development build, the app first looks in its bundled `tools/` directory, then (debug builds only) searches `PATH`, `/opt/homebrew/bin` and `/usr/local/bin`. That is why a development build can work with tools installed by Homebrew. A packaged release does not search the user's `PATH`: it needs the tools bundled with the app, so it works offline and does not depend on Homebrew or end-user setup.

The tools provide these features:

| Tool | Used for |
| --- | --- |
| `exiftool` | Reading photo/video metadata and capture dates |
| `dcraw_emu` | Decoding RAW photos |
| `heif-convert` | Decoding HEIC/HEIF photos |
| `ffmpeg` | Video previews, frame extraction and thumbnail decoding |
| `ffprobe` | Inspecting video streams and metadata |

Release builds require the five executables and the generated manifest to be present; `build.rs` stops the build if any are missing. That presence check cannot guarantee all transitive libraries and runtimes are complete, so smoke-test the packaged app too. Check **Settings → Media tools** in development to see which tools the current machine can find.

### Prepare a release bundle

#### Automatic setup (macOS and Windows x64)

Run the setup on the OS you are building for. [fetch-media.mjs](scripts/fetch-media.mjs) downloads the pinned upstream archives listed in [media-sources.json](scripts/media-sources.json), checks their SHA-256 hashes, and builds the media tools locally. It does not copy or replace your installed Homebrew tools. Windows uses [fetch-media-windows.sh](scripts/fetch-media-windows.sh) under MSYS2 UCRT64 and the upstream Windows ExifTool package, including its runtime.

**macOS:** use native Node.js 22+ (not Rosetta), Xcode Command Line Tools, CMake and pkg-config. If needed, install the build prerequisites with `xcode-select --install` and `brew install cmake pkgconf`. These are build-machine requirements only, not requirements for people using the packaged app.

**Windows x64:** install [MSYS2](https://www.msys2.org/), open its **UCRT64** terminal, update it with `pacman -Syu` (restart the terminal and repeat if instructed), then install:

```sh
pacman -S --needed make curl tar unzip mingw-w64-ucrt-x86_64-gcc \
  mingw-w64-ucrt-x86_64-cmake mingw-w64-ucrt-x86_64-ninja \
  mingw-w64-ucrt-x86_64-pkgconf mingw-w64-ucrt-x86_64-nasm \
  mingw-w64-ucrt-x86_64-nodejs
```

Use an ordinary checkout path without spaces, for example `C:\code\singulo`. Run the following in UCRT64 on Windows, or your normal terminal on macOS:

```sh
cd "$(cygpath -u 'C:\code\singulo')"

npm run fetch:media
npm run prepare:media -- ./media-tools
npm run tauri -- build --no-bundle
```
If you want to build windows installers:

```
npm run tauri -- build 
```


The first command can take several minutes and a few GB of working space. Downloads are cached in the ignored `.media-build/` directory. Successful temporary builds are cleaned up; failed builds are retained there with their path printed for diagnosis. The completed input bundle goes into ignored `media-tools/`, with matching source archives, upstream notices and the build recipe under `licenses/`. An existing output directory is never overwritten; to rebuild, choose a fresh path with `npm run fetch:media -- /path/to/new-media-tools`, then pass that same path to `prepare:media`.

If the Windows build fails with `CreateProcessW failed` and `fork: Permission denied`, close your MSYS2 terminals, reopen UCRT64, and retry with one build job: `SINGULO_MEDIA_JOBS=1 npm run fetch:media`. This starts a fresh build using the cached downloads. If it still fails, make sure MSYS2 is fully updated (`pacman -Syu`, following any restart prompts), and check Windows Security Protection History or your endpoint security logs for a block involving `C:\msys64\usr\bin\sh.exe`.

If a build fails with undefined C++ symbols in `dcraw_half` or `half_mt`, update the checkout and rerun `npm run fetch:media`. Both the macOS and Windows recipes now skip those LibRaw samples and build only `dcraw_emu`, the executable Singulo uses. The retry reuses the downloaded archives; you can remove the failed build directory printed by the script when you no longer need it.

The builds avoid non-system shared libraries: FFmpeg uses Apple's `h264_videotoolbox` encoder on macOS and a source-built `libopenh264` on Windows. The scripts check architecture/dependencies, the preview encoder's presence, HEIC-to-JPEG conversion, video generation/inspection/frame extraction, and ExifTool execution. HEIC support uses libde265; optional AVIF and other HEIF codec plugins are not included. RAW camera support remains subject to LibRaw's supported models. These checks do not replace testing real photos/videos and the final installer on clean target machines. Windows setup must be validated on Windows; it cannot be smoke-tested from a macOS checkout. Windows ARM64 and cross-compilation are not covered. The Tauri app itself still needs its usual Rust/Windows SDK/MSVC build prerequisites; MSYS2 builds the media tools, not the Rust app.

#### Why avoid GPL-enabled FFmpeg?

GPL software is not forbidden, and using it privately does not require publishing your application. This app launches FFmpeg as a separate process rather than linking its libraries; shipping the two together does **not automatically** change Singulo's MIT license. Redistributing GPL-enabled FFmpeg still requires meeting FFmpeg's GPL obligations, including providing the corresponding source in a compliant way.

The non-GPL FFmpeg requirement is this project's conservative **distribution policy**, not a technical limitation or a claim that GPL tools are unsafe. It avoids optional GPL encoders the app does not need. LGPL builds still have license, notice and source obligations; codec patents are a separate issue. The scripts collect source archives and notices to help, but do not certify legal compliance. See [FFmpeg's license explanation](https://ffmpeg.org/legal.html). `--enable-nonfree` is a separate concern: that combination may not be redistributable at all.

#### Supply your own tools

Prepare a directory of verified executables for the **target OS and CPU architecture**. Do not copy tools from a different architecture or assume a development machine's Homebrew installation is suitable for redistribution:

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

The `licenses/` directory is required. Include the applicable notices, corresponding source archives or source locations, and build instructions for every redistributed tool and dependency. Codec patent considerations are separate from open-source licenses.

FFmpeg must be built without `--enable-gpl` or `--enable-nonfree`; enable the H.264 preview encoder (`h264_videotoolbox` on macOS, `libopenh264` on Windows). The preparation script rejects FFmpeg configurations declaring either flag, and rejects known GPL encoder libraries while relocating macOS dependencies. Build libheif without GPL encoder dependencies. A package that happens to run on your machine is not necessarily suitable for redistribution: for example, the default Homebrew FFmpeg may be GPL-enabled.

On macOS, provide ExifTool with its complete Perl module directory; the script copies it into the bundle and adjusts the launcher. It also relocates non-system libraries found with the tools. On Windows, supply the required DLLs and ExifTool runtime files (`exiftool_files`) alongside the executables; the script copies them as-is. The script prepares tools for the platform on which it is run, so cross-platform builds need a separately prepared matching bundle for each target.

```sh
npm run prepare:media -- /absolute/path/to/media-tools
npm run tauri -- build
```

Preparation copies the checked tools into the ignored `src-tauri/resources/tools/` build-resource directory, verifies required executables and FFmpeg configuration, relocates non-system macOS libraries, copies license material, and generates a bundle manifest. Keep the original bundle inputs and their license/source material for future builds. Windows inputs must already include all DLLs/runtime files. Smoke-test each packaged app on a clean target without Homebrew or other media tools installed; preparation is not a substitute for that.

Signing/notarization requires your own certificates/accounts. Unsigned local builds are not notarized. Bundled codecs increase package size; measure the actual installer.
