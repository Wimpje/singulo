# Family Archive

A small, offline desktop app for bringing family photos and videos together. Built with Tauri 2, Rust and Svelte.

## Use it

1. Choose a dedicated archive folder, usually on an external disk.
2. Drop source folders onto the main app or its floating, always-on-top drop window.
3. Browse the library or separate viewer. Search original folder names such as **Holiday Germany**; folder history survives deduplication.
4. Compare uncertain matches in **Similarity review**. Keep both when they are different edits, crops, burst frames or RAW/JPEG companions.

The separate viewer is browsing-only, without import or settings controls. Click a thumbnail for a large photo, use Previous/Next or the arrow keys to browse, and Escape to return to thumbnails. Videos have a Play video button.

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
| MP4/MOV, AVI/MKV/WebM and other recognized videos | Yes | FFmpeg codecs; playback uses a cached proxy |
| XMP sidecars | Yes | Preserved and associated where identifiable |

Unsupported/corrupt previews are explicitly marked; recognized originals can still be copied and exact-deduplicated. RAW/JPEG and identifiable Live Photo companions are protected from automatic replacement.

Video candidates are checked across the entire timeline, with matching frame counts, stream properties and audio. Changed audio, trimmed clips, extra streams, HDR/format differences and uncertain quality go to review. Not every re-encode qualifies automatically.

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

Five focused Rust tests, not an extensive framework. Manually smoke-test import/re-import, matching, viewing, layout changes/undo, recycling on disposable data, and an interrupted operation.

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
