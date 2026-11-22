import { convertFileSrc, invoke } from "@tauri-apps/api/core";

export interface Settings {
  template: string;
  filenames: "original" | "date";
  unknownFolder: string;
  automaticMatches: boolean;
  deepVideoMatching: boolean;
  preventSleepDuringImport: boolean;
  deferPotentialThumbnails: boolean;
}

export interface Media {
  kind: string;
  date: string | null;
  dateSource: string;
  warnings: string[];
  width: number;
  height: number;
  durationSeconds: number | null;
  hash: string;
  bytes: number;
  signature: string | null;
  metadata: Record<string, unknown>;
}

export interface Item {
  id: string;
  path: string;
  originalName: string;
  state: string;
  keeper: string | null;
  media: Media;
  settings: Settings;
  albums: string[];
}

export type PreviewItem = Pick<Item, "id" | "originalName"> & { media: Pick<Media, "kind"> };

export interface Progress {
  jobId?: string | null;
  status: string;
  current: string;
  phase?: string;
  checked?: number;
  batchFiles?: number;
  done: number;
  total: number;
  copied: number;
  duplicates: number;
  errors: string[];
  warnings?: string[];
  folders?: FolderProgress[];
  timings?: ImportTimings;
}

export interface FolderProgress {
  root: string;
  discovered: number;
  processed: number;
  copied: number;
  duplicates: number;
  failed: number;
  scanErrors: number;
  scanComplete: boolean;
}

export interface ImportTimings {
  scanMs: number;
  hashMs: number;
  metadataMs: number;
  bufferMs: number;
  decodeMs: number;
  matchMs: number;
  placementMs: number;
  backupMs: number;
  totalMs: number;
}

export interface Snapshot {
  root: string | null;
  settings: Settings;
  progress: Progress;
  activeCount: number;
  undoCount: number;
  activeBytes: number;
  undoBytes: number;
}

export interface Occurrence {
  id: string;
  source: string;
  relative: string;
  root: string;
  album: string;
  status: string;
  computer: { id: string; name: string } | null;
}

export interface Details {
  item: Item;
  sources: Occurrence[];
  companions: string[];
}

export interface CleanupRow {
  id: string;
  source: string;
  root: string;
  bytes: number;
  eligible: boolean;
  reason: string;
}

export interface ClearImportPreview {
  tracksCopies: boolean;
  removable: number;
  retained: number;
  reasons: string[];
}

export interface FolderSearch {
  root: string;
  running: boolean;
  cancelling: boolean;
  cancelled: boolean;
  current: string;
  directories: number;
  files: number;
  skipped: number;
  issues: string[];
  excluded: number;
  folders: { path: string; count: number; samples: string[] }[];
  error: string | null;
}

export interface MoveRow {
  id: string;
  from: string;
  to: string;
}

export interface Review {
  item: string;
  candidate: string;
  reason: string;
  itemPreview: PreviewItem;
  candidatePreview: PreviewItem;
}

export interface ReviewGroup {
  id: string;
  token: string;
  count: number;
  previews: PreviewItem[];
}

export interface ReviewGroupPage {
  total: number;
  groups: ReviewGroup[];
}

export interface ReviewGroupDetail {
  id: string;
  token: string;
  count: number;
  items: string[];
  reasons: string[];
  links: { numbers: number[]; more: boolean }[];
}

export interface ReviewResult {
  token: string;
  moved: number;
  errors: string[];
  retryable: boolean;
}

export interface Job {
  id: string;
  roots: string[];
  queued?: boolean;
  deferredOnly?: boolean;
  progress: Partial<Progress>;
}

export interface DeferredFolder {
  root: string;
  path: string;
  count: number;
  reason: "named" | "small";
}

export interface CatalogCleanupFolder {
  computer: string;
  computerName: string;
  path: string;
  count: number;
  bytes: number;
  token: string;
  samples: string[];
}

export interface CatalogCleanupResult {
  trashed: number;
  skippedShared: number;
  errors: string[];
}

export interface ToolStatus {
  name: string;
  available: boolean;
  detail: string;
}

export const defaults: Settings = {
  template: "{yyyy}/{mm}/{dd}",
  filenames: "original",
  unknownFolder: "unknown",
  automaticMatches: true,
  deepVideoMatching: false,
  preventSleepDuringImport: false,
  deferPotentialThumbnails: true,
};

export const emptySnapshot: Snapshot = {
  root: null, settings: defaults,
  progress: { status: "idle", current: "", done: 0, total: 0, copied: 0, duplicates: 0, errors: [] },
  activeCount: 0, undoCount: 0, activeBytes: 0, undoBytes: 0,
};

export const api = <T>(command: string, args?: Record<string, unknown>): Promise<T> => invoke<T>(command, args);
export const bytes = (value: number): string => {
  if (value < 1024) return `${value} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let size = value / 1024;
  let index = 0;
  while (size >= 1024 && index < units.length - 1) { size /= 1024; index += 1; }
  return `${size.toFixed(size >= 10 ? 0 : 1)} ${units[index]}`;
};
export const message = (error: unknown): string => error instanceof Error ? error.message : String(error);

let running = 0;
interface QueuedThumb {
  start: () => void;
  cancel: () => void;
}
const waiting: QueuedThumb[] = [];

export async function thumbnail(id: string, large: boolean, signal?: AbortSignal): Promise<string> {
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError");
  if (running >= 4) {
    await new Promise<void>((resolve, reject) => {
      const entry: QueuedThumb = {
        start: () => {
          signal?.removeEventListener("abort", onAbort);
          resolve();
        },
        cancel: () => reject(new DOMException("Aborted", "AbortError")),
      };
      const onAbort = () => {
        const index = waiting.indexOf(entry);
        if (index >= 0) waiting.splice(index, 1);
        entry.cancel();
      };
      signal?.addEventListener("abort", onAbort, { once: true });
      if (large) waiting.unshift(entry);
      else waiting.push(entry);
    });
  }
  if (signal?.aborted) {
    waiting.shift()?.start();
    throw new DOMException("Aborted", "AbortError");
  }
  running += 1;
  try {
    const path = await api<string>("preview_image", { id, large });
    return path.startsWith("data:") ? path : convertFileSrc(path);
  } finally {
    running -= 1;
    waiting.shift()?.start();
  }
}
