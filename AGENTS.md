# Agent Instructions

## Scope & Priorities

Photo Archive (`photo-dedupe`) is a **personal, local-only desktop tool** for consolidating and deduplicating family photos and videos (Tauri 2, Rust, Svelte 5, SQLite). It is **not** a multi-tenant or enterprise production service—avoid enterprise ceremony, over-abstraction, or unnecessary dependencies.

Optimize every change around three priorities, in order:

1. **Data safety for originals** (never lose or silently overwrite user media).
2. **Speed** (fast imports, instant browsing/filtering at 100k+ items, minimal disk I/O on external USB/HDD drives).
3. **User-friendliness** (low-friction workflows, responsive UI, intuitive keyboard/mouse interactions, plain-language copy).

## Testing Policy (Minimal by Design)

- **Keep unit testing very minimal.** Do not chase test coverage, add heavy test frameworks, or write tests for UI components, getters, glue code, or straightforward changes.
- Rely primarily on `npm run check`, `cargo clippy`, and manual smoke-testing in the app.
- Add a focused test in [src-tauri/src/tests.rs](./src-tauri/src/tests.rs) or [tests/comparison.test.mjs](./tests/comparison.test.mjs) **only** when changing subtle data-safety, path-collision, or similarity/comparison rules where a bug could misplace originals or flood reviews.

## User-Friendliness Guidelines

- **Keep interactions effortless:** modals and overlays should close via backdrop click and `Escape`; keyboard navigation (lightbox arrow keys, review shortcuts `1`/`2`/`B`/`S`) should flow naturally without extra clicks. Prefer an Undo message over a confirmation for reversible actions (e.g. keeping a copy moves the other to archive undo).
- **Avoid modal fatigue:** reserve blocking confirmations for destructive or hard-to-reverse actions (such as trashing source files or permanently purging undo copies), and respect opt-outs where provided.
- **Never freeze the UI:** long-running work (imports, video proxy generation, thumbnail decoding) must not block browsing, switching tabs, or closing dialogs. Cancel queued work (e.g. via `AbortController`) when thumbnails scroll out of view.
- **Write calm, clear copy:** explain states and errors in plain human terms rather than raw internal jargon.

## Performance Guardrails

- **Respect external & cloud-backed drives:** keep source reads, hashing, and archive writes sequential on the coordinator thread (`import.rs`) so spinning USB drives do not thrash; keep parallel worker threads strictly for CPU-bound decoding (`media.rs`). Tolerate one-time metadata hydration shifts on cloud placeholders (`OneDrive` / `iCloud` via `hash_with_identity`).
- **Avoid redundant disk passes:** check exact SHA-256 matches before spawning `exiftool` or decoding images, hash bytes while streaming copies (`copy_new`), and use DCT-scaled JPEG / half-size RAW decodes for previews and thumbnails.
- **Scale SQLite to 100k+ items:** keep `journal_mode=WAL`, query via indexed columns/expressions rather than loading full tables (`items` or `occurrences`) into memory, and maintain counts via the `totals` triggers.
- **Keep IPC lean:** serve thumbnails and video proxies from `.photo-dedupe/` via Tauri's `asset:` protocol (`convertFileSrc`) rather than base64 strings. Use read-only helpers (`read` / `inspect_exclusive`) for previews and queries so `"library-changed"` is only emitted when catalog state actually mutates.

## Non-Negotiable Safety Invariants

- **Copy-only imports:** importing must never delete, move, or mutate source files.
- **No silent overwrites:** path collisions always receive a deterministic `__<id>` suffix; archive-relative paths must pass `paths::inside` symlink and cross-platform validation.
- **Reversible deduplication:** replaced lower-resolution copies go to `.photo-dedupe/undo` until explicitly purged. Keep RAW/JPEG pairs, `.xmp` / `.ext.xmp` sidecars, and Live Photo (`ContentIdentifier`) companions protected together.
- **Trash-only source cleanup:** source cleanup must re-verify unchanged source + byte-identical archived originals and move files to native OS Trash / Recycle Bin (`recycle.rs`), never falling back to permanent deletion.

## Codebase Map

- [src/routes/+page.svelte](./src/routes/+page.svelte) — Main app, separate browsing-only `viewer` window, and floating `drop` window UI.
- [src/lib/Preview.svelte](./src/lib/Preview.svelte) & [src/lib/api.ts](./src/lib/api.ts) — Lazy viewport thumbnail loader, priority/abort queue, and Tauri IPC wrappers.
- [src/lib/i18n.svelte.ts](./src/lib/i18n.svelte.ts) — English/Dutch strings (`t()`), reactive language state, and plain-language date-source labels. Add a Dutch entry for every new UI string.
- [src/lib/comparison.js](./src/lib/comparison.js) — Comparison highlight and quality-hint helpers.
- [src-tauri/src/lib.rs](./src-tauri/src/lib.rs) — Tauri command handlers, concurrency guards, and window management.
- [src-tauri/src/archive.rs](./src-tauri/src/archive.rs) — SQLite catalog, WAL/migrations, staged file placement, reorganization/rollback, undo/purge, and recovery backups.
- [src-tauri/src/import.rs](./src-tauri/src/import.rs) — Batched import pipeline, worker pool coordination, and candidate matching.
- [src-tauri/src/media.rs](./src-tauri/src/media.rs) — Hashing, ExifTool batching, JPEG/RAW/HEIC/video decoding, visual signatures, and equivalence checks.
- [src-tauri/src/paths.rs](./src-tauri/src/paths.rs) & [src-tauri/src/recycle.rs](./src-tauri/src/recycle.rs) — Cross-platform path validation, collision naming, and native OS recycling.

## Verification Commands

Run the smallest relevant check after making changes:

```sh
npm run check                                                        # Svelte + TypeScript check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm test                                                             # Minimal JS + Rust checks
npm run build                                                        # Static frontend build
```
