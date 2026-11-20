<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { isTauri, convertFileSrc } from "@tauri-apps/api/core";
  import { getVersion } from "@tauri-apps/api/app";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open, confirm } from "@tauri-apps/plugin-dialog";
  import Preview from "$lib/Preview.svelte";
  import { comparePictures } from "$lib/comparison";
  import { locale, t, dateSourceLabel } from "$lib/i18n.svelte";
  import {
    api, bytes, defaults, emptySnapshot, message,
    type Snapshot, type Settings, type Progress, type Item, type Details,
    type Review, type CleanupRow, type ClearImportPreview, type MoveRow, type Job, type ToolStatus,
  } from "$lib/api";

  type Tab = "library" | "review" | "import" | "settings";
  type LibraryMonth = { month: string; count: number };
  type VideoPlayback = { id: string; request: number; source: string; phase: "opening" | "original" | "converting" | "proxy" };
  type UndoAction = { review: Review; discarded: string; keptName: string };
  type CleanupFolder = { root: string; rows: CleanupRow[]; eligible: string[] };
  type XmpSummary = { created: number; updated: number; removed: number; skipped: number; errors: string[] };
  type DateRepairSummary = { updated: number; skipped: number; errors: string[] };
  const pageSize = 60;
  const textScales = [0.9, 1, 1.1, 1.2, 1.35, 1.5];
  let textScale = $state(storedTextScale());
  let highContrast = $state(window.localStorage.getItem("singulo.highContrast") === "true");
  let native = $state(false);
  let appVersion = $state("");
  let mode = $state("main");
  let tab = $state<Tab>("library");
  let snapshot = $state<Snapshot>(structuredClone(emptySnapshot));
  let config = $state<Settings>({ ...defaults });
  let error = $state("");
  let notice = $state("");
  let working = $state(false);
  let dropping = $state(false);
  let items = $state<Item[]>([]);
  let itemsStart = $state(0);
  let reviews = $state<Review[]>([]);
  let jobs = $state<Job[]>([]);
  let tools = $state<ToolStatus[]>([]);
  let search = $state("");
  let filter = $state("all");
  let months = $state<LibraryMonth[]>([]);
  let month = $state("");
  let libraryLoading = $state(false);
  let loadingMore = $state(false);
  let examples = $state<string[]>([]);
  let configError = $state("");
  let selected = $state<Details | null>(null);
  let comparing = $state<Details | null>(null);
  let currentReview = $state<Review | null>(null);
  let reviewIndex = $state(0);
  let opening = $state(false);
  let showInfo = $state(true);
  let thumbSize = $state(200);
  let zoom = $state<{ x: number; y: number } | null>(null);
  let undoAction = $state<UndoAction | null>(null);
  let undoTimer: ReturnType<typeof setTimeout> | undefined;
  let dateEntry = $state("");
  let videos = $state<(VideoPlayback | null)[]>([null, null]);
  let videoRequest = 0;
  let firstVideo = $state<HTMLVideoElement>();
  let secondVideo = $state<HTMLVideoElement>();
  let cleanup = $state<CleanupRow[] | null>(null);
  let cleanupLoading = $state(false);
  let cleanupError = $state("");
  let cleanupFileFolders = $state<string[]>([]);
  let cleanupFileLimits = $state<Record<string, number>>({});
  let cleanupSelection = $state<string[]>([]);
  let emptyFolders = $state(false);
  let cleanupResults = $state<string[]>([]);
  let clearingJob = $state<Job | null>(null);
  let clearPreview = $state<ClearImportPreview | null>(null);
  let clearLoading = $state(false);
  let clearError = $state("");
  let trashImportCopies = $state(false);
  let moves = $state<MoveRow[] | null>(null);
  let undoingMoves = $state(false);
  let xmpResult = $state<{ root: string; summary: XmpSummary } | null>(null);
  let xmpAttention = $state<string[]>([]);
  let onTop = $state(true);
  let loaded = $state(false);
  let revealLabel = $state("Show in folder");
  let lastQuery = "";
  let searchSeq = 0;
  let detailSeq = 0;
  let mutating = false;
  let moreRequest: Promise<void> | null = null;
  let jobsRequest = 0;
  let progressRevision = 0;
  let importLibraryJob = "";
  let importLibraryCopied = 0;
  let importLibraryTimer: ReturnType<typeof setTimeout> | undefined;
  let stageProgress = $state<Progress>(emptySnapshot.progress);
  let progressClock = $state(Date.now());
  let progressStartedAt = $state(Date.now());
  const importStatus = $derived(snapshot.progress.status);
  const cancelling = $derived(importStatus === "cancelling");
  const importing = $derived(cancelling || ["scanning", "importing", "paused"].includes(snapshot.progress.status));
  const progressKey = $derived(importing ? snapshot.progress.jobId ?? "active" : null);
  const busy = $derived(working || importing || cleanupLoading || clearLoading);
  const retryBlocked = $derived(working || cancelling || cleanupLoading || clearLoading);
  const displayedStage = $derived(stageProgress.jobId === snapshot.progress.jobId && stageProgress.status === importStatus
    ? stageProgress : snapshot.progress);
  const hasArchive = $derived(!!snapshot.root);
  const percent = $derived(snapshot.progress.total ? Math.round(snapshot.progress.done / snapshot.progress.total * 100) : 0);
  const determinate = $derived(snapshot.progress.status !== "scanning" && snapshot.progress.total > 0 && (!importing || snapshot.progress.done > 0));
  const elapsedMs = $derived(importing ? Math.max(snapshot.progress.timings?.totalMs ?? 0, progressClock - progressStartedAt) : snapshot.progress.timings?.totalMs ?? 0);
  const queuedJobs = $derived(jobs.filter((job) => job.queued && job.id !== snapshot.progress.jobId).reverse());
  const phaseTitle = $derived.by(() => {
    if (cancelling) return t("Cancelling import...");
    if (snapshot.progress.status === "paused") return t("Import paused");
    if (snapshot.progress.status === "scanning") return t("Finding photos and videos...");
    switch (displayedStage.phase) {
      case "hashing": return t("Reading originals and checking exact copies...");
      case "metadata": return t("Reading dates and camera details...");
      case "raw": return t("Developing a RAW comparison preview...");
      case "decoding": return t("Preparing photo and video previews...");
      case "saving": return t("Comparing and safely saving files...");
      case "finalizing": return t("Finishing sidecars and the recovery backup...");
      default: return t("Preparing your import...");
    }
  });
  const phaseDetail = $derived.by(() => {
    if (cancelling) return t("Waiting for the current operation to stop safely. Your source files are unchanged.");
    if (snapshot.progress.status === "paused") return t("Resume or cancel the import to make these actions available again.");
    if (snapshot.progress.status === "scanning") return `${(snapshot.progress.folders ?? []).reduce((total, folder) => total + folder.discovered, 0).toLocaleString()} ${t("recognized media files found")}`;
    if (displayedStage.phase === "hashing") return `${(displayedStage.checked ?? 0).toLocaleString()} / ${displayedStage.total.toLocaleString()} ${t("originals checked")}`;
    if (displayedStage.phase === "metadata") return `${displayedStage.batchFiles ?? 0} ${t("files in this metadata batch")}`;
    if (displayedStage.phase === "raw") return t("RAW files such as PEF need decoding before they can be compared. Your original stays unchanged.");
    return "";
  });
  const activityTitle = $derived(importing
    ? t(cancelling ? "Cancelling import..." : snapshot.progress.status === "scanning" ? "Scanning source folders..." : snapshot.progress.status === "paused" ? "Import paused" : "Importing photos and videos...")
    : cleanupLoading ? t("Checking source files...")
    : clearLoading ? t("Checking imported copies...")
    : t(loaded ? "Updating your archive..." : "Opening your archive..."));
  const busyReason = $derived(!busy ? undefined : importing
    ? t("Comparisons, source cleanup and archive changes wait until the import finishes to avoid conflicting with files being added or matched.")
    : cleanupLoading || clearLoading ? t("Files are being checked. You can keep browsing; archive changes will be available when checking finishes.")
    : t(loaded
      ? "An archive operation is in progress. Please wait for it to finish before making other changes."
      : "Please wait while your archive opens. Archive changes will be available when loading finishes."));
  const reviewDisabledReason = $derived(busyReason ?? (opening ? t("Opening comparison...") : undefined));
  const viewerIndex = $derived(selected ? items.findIndex((item) => item.id === selected?.item.id) : -1);
  const absoluteIndex = $derived(viewerIndex < 0 ? -1 : itemsStart + viewerIndex);
  const pictureComparison = $derived(selected && comparing ? comparePictures(selected.item.media, comparing.item.media) : null);
  const monthCount = $derived(months.find((entry) => entry.month === month)?.count ?? 0);
  const monthPosition = $derived(months.findIndex((entry) => entry.month === month));
  const year = $derived(month === "unknown" ? "unknown" : month.slice(0, 4));
  const years = $derived.by(() => {
    const result: { year: string; count: number }[] = [];
    for (const entry of months) {
      const value = entry.month === "unknown" ? "unknown" : entry.month.slice(0, 4);
      const last = result.at(-1);
      if (last?.year === value) last.count += entry.count;
      else result.push({ year: value, count: entry.count });
    }
    return result;
  });
  const canPrevious = $derived(absoluteIndex > 0 || (absoluteIndex === 0 && monthPosition > 0));
  const canNext = $derived(absoluteIndex >= 0 && (absoluteIndex < monthCount - 1 || monthPosition < months.length - 1));
  const settingsDirty = $derived((Object.keys(defaults) as (keyof Settings)[]).some((key) => config[key] !== snapshot.settings[key]));
  const filtered = $derived(search.trim() !== "" || filter !== "all");
  const incompleteJobs = $derived(jobs.filter(canRetry));
  const cleanupSelected = $derived(new Set(cleanupSelection));
  const cleanupBytes = $derived((cleanup ?? []).reduce((total, row) => total + (cleanupSelected.has(row.id) ? row.bytes : 0), 0));
  const cleanupFolders = $derived.by(() => {
    const groups = new Map<string, CleanupFolder>();
    for (const row of cleanup ?? []) {
      let group = groups.get(row.root);
      if (!group) {
        group = { root: row.root, rows: [], eligible: [] };
        groups.set(row.root, group);
      }
      group.rows.push(row);
      if (row.eligible) group.eligible.push(row.id);
    }
    return [...groups.values()];
  });
  const presets = [
    ["{yyyy}/{mm}/{dd}", "Year / month / day"],
    ["{yyyy}/{mm}", "Year / month"],
    ["{yyyy}-{mm}-{dd}", "Date folders"],
    ["{media_type}/{yyyy}/{mm}/{dd}", "Photos & videos / year / month / day"],
  ];

  $effect(() => {
    document.documentElement.lang = locale.language;
  });

  $effect(() => {
    if (!progressKey) return;
    const now = Date.now();
    progressClock = now;
    progressStartedAt = now - untrack(() => snapshot.progress.timings?.totalMs ?? 0);
    const timer = setInterval(() => { progressClock = Date.now(); }, 1000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    if (!progressKey || !importStatus) return;
    // Sample the stage, its details, and its path together. Counts and control states stay live.
    const sample = () => { stageProgress = snapshot.progress; };
    untrack(sample);
    const timer = setInterval(sample, 1000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    document.body.style.overflow = selected && !comparing ? "hidden" : "";
  });

  function changeLanguage(event: Event) {
    locale.language = (event.currentTarget as HTMLSelectElement).value === "nl" ? "nl" : "en";
    window.localStorage.setItem("singulo.language", locale.language);
  }

  function syncPreferences(event: StorageEvent) {
    if (event.key === "singulo.language") locale.language = event.newValue === "nl" ? "nl" : "en";
    if (event.key === "singulo.textScale") textScale = storedTextScale();
    if (event.key === "singulo.highContrast") highContrast = event.newValue === "true";
  }

  function archiveLabel(root: string) {
    const parts = root.split(/[\\/]/).filter(Boolean);
    const name = parts.at(-1) ?? root;
    const volume = parts[0] === "Volumes" && parts.length > 2 ? parts[1] : /^[A-Za-z]:$/.test(parts[0] ?? "") && parts.length > 1 ? parts[0] : "";
    return volume ? `${volume} › ${name}` : name;
  }

  function storedTextScale() {
    const stored = Number(window.localStorage.getItem("singulo.textScale"));
    return textScales.includes(stored) ? stored : 1;
  }

  function setTextScale(value: number) {
    textScale = value;
    window.localStorage.setItem("singulo.textScale", String(value));
  }

  function stepTextScale(direction: -1 | 1) {
    const index = Math.max(0, textScales.indexOf(textScale));
    setTextScale(textScales[Math.min(textScales.length - 1, Math.max(0, index + direction))]);
  }

  function toggleContrast() {
    highContrast = !highContrast;
    window.localStorage.setItem("singulo.highContrast", String(highContrast));
  }

  $effect.pre(() => {
    document.documentElement.style.setProperty("--text-scale", String(textScale));
    document.documentElement.classList.toggle("high-contrast", highContrast);
  });

  function monthLabel(value: string, style: "long" | "short" = "long") {
    if (!value) return "";
    if (value === "unknown") return t("Unknown date");
    const [y, m] = value.split("-").map(Number);
    return new Date(Date.UTC(y, m - 1, 1)).toLocaleDateString(locale.language === "nl" ? "nl-NL" : "en-US", style === "long"
      ? { month: "long", year: "numeric", timeZone: "UTC" }
      : { month: "short", timeZone: "UTC" });
  }

  function formatDate(value: string | null) {
    if (!value) return t("Date unknown");
    const [y, m, d] = value.slice(0, 10).split("-").map(Number);
    if (!y || !m || !d) return value;
    const date = new Date(Date.UTC(y, m - 1, d)).toLocaleDateString(locale.language === "nl" ? "nl-NL" : "en-US", { day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });
    const time = value.slice(11, 16);
    return /^\d\d:\d\d$/.test(time) ? `${date}, ${time}` : date;
  }

  // Jobs are newest first. Unfinished statuses belong to the running import, or to one interrupted by a quit.
  // Job history already hides attempts superseded by a later import of the same folders.
  function canRetry(job: Job) {
    const status = job.progress.status ?? "queued";
    if (status === "complete") return false;
    return !job.queued && !(importing && job.id === snapshot.progress.jobId);
  }

  function jobStatus(job: Job) {
    const status = job.progress.status ?? "queued";
    return !job.queued && (!importing || job.id !== snapshot.progress.jobId)
      && ["scanning", "importing", "paused", "cancelling", "queued"].includes(status) ? "interrupted" : status;
  }

  function retryReason(job: Job) {
    if (retryBlocked) return busyReason;
    if (jobs.some((other) => (other.queued || (importing && other.id === snapshot.progress.jobId))
      && job.roots.every((root) => other.roots.includes(root)))) {
      return t("These folders are already queued or importing.");
    }
  }

  function jobExplanation(job: Job) {
    const status = jobStatus(job);
    if (status === "cancelled") return t("Import canceled. Files that were not reached are still only in your source folders.");
    if (status === "interrupted") return t("This import stopped before completion, possibly when the app closed or the disk disconnected.");
    if (status === "failed") return t("Import stopped because of an error. Some files may not have been copied.");
    return t("Some files or folders could not be imported. Review the issues below and retry.");
  }

  function focusOnMount(node: HTMLElement) {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    node.focus({ preventScroll: true });
    return () => { if (previous?.isConnected) previous.focus({ preventScroll: true }); };
  }

  async function run(action: () => Promise<void>) {
    working = true;
    error = "";
    notice = "";
    try { await action(); }
    catch (reason: unknown) { error = message(reason); console.error(reason); }
    finally { working = false; }
  }

  async function readSnapshot() {
    const revision = progressRevision;
    const next = await api<Snapshot>("snapshot");
    // Don't overwrite a newer live progress event with a snapshot that was already in flight.
    if (next.root === snapshot.root && revision !== progressRevision) next.progress = snapshot.progress;
    snapshot = next;
  }

  function refreshImportedPhotos(progress: Progress) {
    if (mode === "drop") return;
    const job = progress.jobId ?? "";
    if (job !== importLibraryJob) {
      importLibraryJob = job;
      importLibraryCopied = 0;
    }
    const added = progress.copied > importLibraryCopied;
    importLibraryCopied = progress.copied;
    if (!added || !loaded || !snapshot.root || importLibraryTimer) return;
    importLibraryTimer = setTimeout(() => {
      importLibraryTimer = undefined;
      void readSnapshot()
        .then(() => loadLibrary(true))
        .catch((reason: unknown) => { error = message(reason); });
    }, 1000);
  }

  async function refresh(preservePages = false) {
    await readSnapshot();
    if (snapshot.root) {
      if (mode === "main") {
        [reviews] = await Promise.all([api<Review[]>("review_list"), refreshJobs()]);
      }
      await loadLibrary(preservePages);
    } else {
      items = [];
      itemsStart = 0;
      months = [];
      month = "";
      lastQuery = "";
      libraryLoading = false;
      searchSeq += 1;
    }
  }

  async function refreshJobs() {
    const request = ++jobsRequest;
    const root = snapshot.root;
    const result = await api<Job[]>("job_history");
    if (request === jobsRequest && root === snapshot.root) jobs = result;
  }

  async function mutate(action: () => Promise<void>) {
    mutating = true;
    try { await action(); }
    finally { mutating = false; }
  }

  async function fetchRange(term: string, kind: string, target: string, start: number, end: number) {
    const offsets: number[] = [];
    for (let offset = start; offset < end; offset += pageSize) offsets.push(offset);
    if (!offsets.length) offsets.push(start);
    const pages = await Promise.all(offsets.map((offset) => api<Item[]>("library", { search: term, filter: kind, month: target, offset })));
    return pages.flat();
  }

  async function loadLibrary(preservePage = false, seq = ++searchSeq) {
    const key = `${search}\0${filter}`;
    const currentSearch = search;
    const currentFilter = filter;
    const result = await api<LibraryMonth[]>("library_months", { search: currentSearch, filter: currentFilter });
    if (seq !== searchSeq) return;
    const nextMonth = result.some((entry) => entry.month === month) ? month : result[0]?.month ?? "";
    const count = result.find((entry) => entry.month === nextMonth)?.count ?? 0;
    // Keep the loaded window when the catalog changes underneath, so scrolling and the open photo stay put.
    const keep = preservePage && key === lastQuery && nextMonth === month;
    const start = keep ? Math.min(itemsStart, Math.max(0, Math.floor((count - 1) / pageSize) * pageSize)) : 0;
    const end = Math.min(count, keep ? Math.max(itemsStart + items.length, start + pageSize) : pageSize);
    const nextItems = nextMonth ? await fetchRange(currentSearch, currentFilter, nextMonth, start, end) : [];
    if (seq !== searchSeq) return;
    months = result;
    month = nextMonth;
    itemsStart = start;
    items = nextItems;
    lastQuery = key;
    libraryLoading = false;
  }

  async function changeMonth(nextMonth: string, at: "start" | "end" = "start", scroll = true) {
    // Ignore month changes while a new search/filter is still loading; its months list isn't ready yet.
    if (!nextMonth || `${search}\0${filter}` !== lastQuery) return false;
    const seq = ++searchSeq;
    const count = months.find((entry) => entry.month === nextMonth)?.count ?? 0;
    const start = at === "end" ? Math.max(0, Math.floor((count - 1) / pageSize) * pageSize) : 0;
    libraryLoading = true;
    try {
      const result = await api<Item[]>("library", { search, filter, month: nextMonth, offset: start });
      if (seq !== searchSeq) return false;
      month = nextMonth;
      itemsStart = start;
      items = result;
      if (scroll) window.scrollTo({ top: 0 });
      return true;
    } finally {
      if (seq === searchSeq) libraryLoading = false;
    }
  }

  function loadMore(): Promise<void> {
    if (moreRequest) return moreRequest;
    const offset = itemsStart + items.length;
    if (!month || offset >= monthCount) return Promise.resolve();
    const seq = searchSeq;
    const current = month;
    loadingMore = true;
    moreRequest = api<Item[]>("library", { search, filter, month: current, offset })
      .then((result) => {
        if (seq === searchSeq && month === current && itemsStart + items.length === offset) items = [...items, ...result];
      })
      .catch((reason: unknown) => { error = message(reason); })
      .finally(() => { moreRequest = null; loadingMore = false; });
    return moreRequest;
  }

  async function loadEarlier() {
    if (itemsStart <= 0 || loadingMore) return;
    const seq = searchSeq;
    const current = month;
    const offset = Math.max(0, itemsStart - pageSize);
    loadingMore = true;
    try {
      const result = await api<Item[]>("library", { search, filter, month: current, offset });
      if (seq !== searchSeq || month !== current) return;
      items = [...result.slice(0, itemsStart - offset), ...items];
      itemsStart = offset;
    } catch (reason: unknown) { error = message(reason); }
    finally { loadingMore = false; }
  }

  function loadOnScroll(node: HTMLElement) {
    let active = true;
    const check = async () => {
      const before = items.length;
      await loadMore();
      if (active && items.length > before && node.getBoundingClientRect().top < window.innerHeight + 800) void check();
    };
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) void check();
    }, { rootMargin: "800px" });
    observer.observe(node);
    return () => { active = false; observer.disconnect(); };
  }

  function clearSearch() {
    search = "";
    filter = "all";
  }

  function updateThumbSize() {
    window.localStorage.setItem("singulo.thumbSize", String(thumbSize));
  }

  // Opened outside run() so the busy status only appears once folders have been chosen.
  async function chooseFolders(title: string, multiple: boolean) {
    try {
      const chosen = await open({ directory: true, multiple, title });
      return typeof chosen === "string" ? [chosen] : chosen;
    } catch (reason: unknown) {
      error = message(reason);
      return null;
    }
  }

  async function chooseArchive() {
    const [path] = await chooseFolders(t("Choose the archive folder, ideally on an external disk"), false) ?? [];
    if (!path) return;
    await run(async () => {
      await mutate(async () => {
        snapshot = await api<Snapshot>("open_archive", { path });
        xmpAttention = [];
        config = { ...snapshot.settings };
        await refresh();
      });
      loaded = true;
    });
  }

  async function importFolders(paths?: string[]) {
    if (cancelling) {
      notice = t("Waiting for the current operation to stop safely. Your source files are unchanged.");
      return;
    }
    if (!snapshot.root) {
      await chooseArchive();
      if (!snapshot.root) return;
    }
    const folders = paths ?? await chooseFolders(t("Choose photo folders to copy"), true);
    if (!folders?.length) return;
    await run(async () => {
      const started = await api<boolean>("start_import", { roots: folders });
      notice = started ? t("Folders added. Originals stay untouched during import.") : t("Folders queued after the current import.");
      await refreshJobs();
    });
  }

  function showDetail(detail: Details) {
    selected = detail;
    comparing = null;
    currentReview = null;
    zoom = null;
    dateEntry = detail.item.media.date?.slice(0, 10) ?? "";
    resetVideos();
  }

  async function openItem(id: string) {
    const seq = ++detailSeq;
    opening = true;
    try {
      const detail = await api<Details>("item_details", { id });
      if (seq === detailSeq) showDetail(detail);
    } catch (reason: unknown) { error = message(reason); }
    finally { if (seq === detailSeq) opening = false; }
  }

  async function navigatePhoto(direction: -1 | 1) {
    if (opening || working || !selected || comparing || absoluteIndex < 0) return;
    const target = absoluteIndex + direction;
    const seq = ++detailSeq;
    opening = true;
    error = "";
    try {
      let item: Item | undefined;
      if (target >= 0 && target < monthCount) {
        if (target === itemsStart + items.length) await loadMore();
        else if (target === itemsStart - 1) await loadEarlier();
        item = items[target - itemsStart];
      } else {
        const next = months[monthPosition + direction];
        if (!next) return;
        if (!await changeMonth(next.month, direction < 0 ? "end" : "start", false) || month !== next.month) {
          throw new Error(t("The photo list changed. Refresh the library and try again."));
        }
        item = direction < 0 ? items.at(-1) : items[0];
      }
      if (!item) throw new Error(t("The photo list changed. Refresh the library and try again."));
      const detail = await api<Details>("item_details", { id: item.id });
      if (seq === detailSeq) showDetail(detail);
    } catch (reason: unknown) { error = message(reason); }
    finally { if (seq === detailSeq) opening = false; }
  }

  async function reveal(item: Item) {
    error = "";
    try { await api("reveal_item", { id: item.id }); }
    catch (reason: unknown) { error = message(reason); }
  }

  function onKey(event: KeyboardEvent) {
    const target = event.target instanceof HTMLElement ? event.target : null;
    const typing = !!target && (target.isContentEditable || !!target.closest("input, select, textarea"));
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "z" && undoAction && !typing && !busy) {
      event.preventDefault();
      void undoKeep();
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === "Escape") {
      if (working) return;
      if (clearingJob) clearingJob = null;
      else if (moves !== null) moves = null;
      else if (cleanup !== null) cleanup = null;
      else if (zoom) zoom = null;
      else if (selected) closeDetails();
      else return;
      event.preventDefault();
      return;
    }
    if (typing || target?.closest("video") || cleanup !== null || clearingJob !== null || moves !== null || working) return;
    if (comparing && currentReview) {
      reviewKey(event);
      return;
    }
    if (!selected) return;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      void navigatePhoto(event.key === "ArrowLeft" ? -1 : 1);
    } else if (event.key.toLowerCase() === "i") {
      toggleInfo();
    }
  }

  function toggleInfo() {
    showInfo = !showInfo;
    window.localStorage.setItem("singulo.showInfo", String(showInfo));
  }

  function reviewKey(event: KeyboardEvent) {
    if (opening || busy) return;
    const key = event.key.toLowerCase();
    // Never let a held-down key resolve several matches the user hasn't seen.
    if (event.repeat && (key === "1" || key === "2" || key === "b")) return;
    if (key === "1" || key === "2") {
      const index = key === "1" ? 0 : 1;
      const detail = index === 0 ? selected : comparing;
      const other = index === 0 ? comparing : selected;
      if (detail && other && !other.companions.length) void resolve(detail.item.id);
    } else if (key === "b") void resolve(null);
    else if (key === "s" || event.key === "ArrowRight") void stepReview(1);
    else if (event.key === "ArrowLeft") void stepReview(-1);
    else if (key === "z") zoom = zoom ? null : { x: 50, y: 50 };
    else return;
    event.preventDefault();
  }

  async function openReview(index: number) {
    const review = reviews[index];
    if (!review) return;
    const seq = ++detailSeq;
    opening = true;
    try {
      const [first, second] = await Promise.all([
        api<Details>("item_details", { id: review.item }),
        api<Details>("item_details", { id: review.candidate }),
      ]);
      if (seq !== detailSeq) return;
      selected = first;
      comparing = second;
      currentReview = review;
      reviewIndex = index;
      zoom = null;
      resetVideos();
    } catch (reason: unknown) { error = message(reason); }
    finally { if (seq === detailSeq) opening = false; }
  }

  async function stepReview(direction: -1 | 1) {
    if (!reviews.length) return;
    await openReview((reviewIndex + direction + reviews.length) % reviews.length);
  }

  async function resolve(keeper: string | null) {
    const review = currentReview;
    if (!review || !selected || !comparing || working) return;
    const index = reviewIndex;
    const kept = keeper === selected.item.id ? selected : keeper === comparing.item.id ? comparing : null;
    const discarded = keeper ? (keeper === review.item ? review.candidate : review.item) : null;
    await run(async () => {
      await mutate(async () => {
        await api("resolve_review", { item: review.item, candidate: review.candidate, keeper });
        await refresh(true);
      });
      if (discarded && kept) showUndo({ review, discarded, keptName: kept.item.originalName });
      if (reviews.length) await openReview(Math.min(index, reviews.length - 1));
      else {
        closeDetails();
        notice = t("All similar matches have been reviewed.");
      }
    });
  }

  function showUndo(action: UndoAction) {
    clearTimeout(undoTimer);
    undoAction = action;
    undoTimer = setTimeout(() => { undoAction = null; }, 12000);
  }

  async function undoKeep() {
    const action = undoAction;
    if (!action) return;
    clearTimeout(undoTimer);
    undoAction = null;
    await run(async () => {
      await mutate(async () => {
        await api("undo_review", { item: action.review.item, candidate: action.review.candidate, reason: action.review.reason, discarded: action.discarded });
        await refresh(true);
      });
      const index = reviews.findIndex((review) => review.item === action.review.item && review.candidate === action.review.candidate);
      if (index >= 0) {
        tab = "review";
        await openReview(index);
      }
      notice = t("Restored. The match is back in review.");
    });
  }

  function zoomAt(event: MouseEvent) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    zoom = zoom ? null : { x: (event.clientX - rect.left) / rect.width * 100, y: (event.clientY - rect.top) / rect.height * 100 };
  }

  function panZoom(event: MouseEvent) {
    if (!zoom) return;
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    zoom = {
      x: Math.min(100, Math.max(0, (event.clientX - rect.left) / rect.width * 100)),
      y: Math.min(100, Math.max(0, (event.clientY - rect.top) / rect.height * 100)),
    };
  }

  function resetVideos() {
    videos = [null, null];
  }

  async function playVideo(item: Item, index = 0) {
    const request = ++videoRequest;
    videos[index] = { id: item.id, request, source: "", phase: "opening" };
    error = "";
    try {
      const path = await api<string>("video_original", { id: item.id });
      if (videos[index]?.request === request) {
        videos[index] = { id: item.id, request, source: convertFileSrc(path), phase: "original" };
      }
    } catch (reason: unknown) {
      if (videos[index]?.request !== request) return;
      videos[index] = null;
      error = message(reason);
      console.error(reason);
    }
  }

  async function videoFailed(index: number, id: string, source: string) {
    const video = videos[index];
    if (!video || video.id !== id || video.source !== source) return;
    if (video.phase === "proxy") {
      videos[index] = null;
      error = t("This video could not be played, even with a playable copy.");
      return;
    }
    if (video.phase !== "original") return;
    videos[index] = { ...video, phase: "converting", source: "" };
    try {
      const path = await api<string>("video_preview", { id });
      if (videos[index]?.request === video.request) {
        videos[index] = { ...video, source: convertFileSrc(path), phase: "proxy" };
      }
    } catch (reason: unknown) {
      if (videos[index]?.request !== video.request) return;
      videos[index] = null;
      error = message(reason);
      console.error(reason);
    }
  }

  async function startCleanup() {
    cleanup = [];
    cleanupLoading = true;
    cleanupError = "";
    error = "";
    cleanupSelection = [];
    cleanupResults = [];
    cleanupFileFolders = [];
    cleanupFileLimits = {};
    await tick();
    try {
      const rows = await api<CleanupRow[]>("cleanup_preview");
      if (cleanup === null) return;
      cleanup = rows;
      cleanupSelection = rows.filter((row) => row.eligible).map((row) => row.id);
    } catch (reason: unknown) {
      if (cleanup !== null) cleanupError = message(reason);
      else error = message(reason);
      console.error(reason);
    } finally { cleanupLoading = false; }
  }

  function toggleCleanupFolder(folder: CleanupFolder, checked: boolean) {
    const selected = new Set(cleanupSelection);
    for (const id of folder.eligible) {
      if (checked) selected.add(id);
      else selected.delete(id);
    }
    cleanupSelection = [...selected];
  }

  async function startClearImport(job: Job) {
    clearingJob = job;
    clearPreview = null;
    clearLoading = true;
    clearError = "";
    trashImportCopies = false;
    await tick();
    try {
      const preview = await api<ClearImportPreview>("clear_import_preview", { id: job.id });
      if (clearingJob?.id === job.id) clearPreview = preview;
    } catch (reason: unknown) {
      if (clearingJob?.id === job.id) clearError = message(reason);
      else error = message(reason);
      console.error(reason);
    } finally { clearLoading = false; }
  }

  async function clearImport() {
    const job = clearingJob;
    if (!job || !clearPreview) return;
    await run(async () => {
      clearError = "";
      try {
        await mutate(async () => {
          await api("clear_import", { id: job.id, trashCopies: trashImportCopies, confirmed: true });
        });
        clearingJob = null;
        notice = t("Import cleared from history. Source folders were not changed.");
      } catch (reason: unknown) {
        clearError = message(reason);
        console.error(reason);
      } finally {
        await refresh(true);
        if (clearingJob?.id === job.id) {
          clearPreview = await api<ClearImportPreview>("clear_import_preview", { id: job.id });
        }
      }
    });
  }

  async function cleanSources() {
    await run(async () => {
      if (!cleanupSelection.length) throw new Error(t("Select at least one eligible file."));
      const cleanupPrompt = locale.language === "nl"
        ? `${cleanupSelection.length} gecontroleerde bronbestanden naar de prullenmand verplaatsen?\n\nDaarna is je archief misschien de enige kopie. Bewaar daarom ook een tweede back-up.\n\nLeeg de prullenmand zelf om schijfruimte vrij te maken. De app verwijdert bronbestanden nooit definitief.`
        : `Move ${cleanupSelection.length} verified source files to OS Trash/Recycle Bin?\n\nThis may leave your archive as the only copy. A second backup is recommended.\n\nTrash must usually be emptied manually before disk space is reclaimed. The app never permanently deletes sources.`;
      const approved = await confirm(
        cleanupPrompt,
        { title: t("Clean up verified sources"), kind: "warning" },
      );
      if (!approved) return;
      await mutate(async () => {
        cleanupResults = await api<string[]>("clean_sources", { ids: cleanupSelection, confirmed: true, emptyFolders });
        cleanup = await api<CleanupRow[]>("cleanup_preview");
        cleanupSelection = [];
        await refresh(true);
      });
    });
  }

  async function previewMoves() {
    await run(async () => {
      moves = await api<MoveRow[]>("reorganization_preview");
      undoingMoves = false;
    });
  }

  async function repairDates() {
    await run(async () => {
      const prompt = t("Find dates in original filenames and folders, then move affected archive files to their matching folders? Camera and manually corrected dates stay unchanged. Source files are never changed. This cannot be undone automatically and may take a while.");
      if (!await confirm(prompt, { title: t("Correct estimated dates"), kind: "warning" })) return;
      await mutate(async () => {
        const result = await api<DateRepairSummary>("repair_estimated_dates", { confirmed: true });
        await refresh(true);
        notice = `${t("Dates corrected")}: ${result.updated} · ${t("Skipped for safety")}: ${result.skipped}`;
        if (result.errors.length) error = result.errors.join("\n");
      });
    });
  }

  async function applyMoves() {
    await run(async () => {
      const movePrompt = locale.language === "nl"
        ? `${moves?.length ?? 0} bestanden in je archief verplaatsen volgens de opgeslagen indeling? Je bronbestanden blijven ongewijzigd. Het archief onthoudt de oude plekken, zodat je dit kunt terugdraaien.`
        : `Reorganize ${moves?.length ?? 0} archived files using the saved layout? Sources are untouched. A recovery manifest is saved with the archive.`;
      if (!moves?.length || !await confirm(movePrompt, { title: t("Reorganize archive"), kind: "warning" })) return;
      await mutate(async () => {
        await api(undoingMoves ? "rollback_archive" : "reorganize_archive", { preview: moves, confirmed: true });
        moves = null;
        await refresh();
      });
      notice = undoingMoves ? t("Reorganization undone. Original folder history is preserved.") : t("Archive reorganized. Original folder history is preserved.");
    });
  }

  async function restore(item: Item) {
    await run(async () => {
      await mutate(async () => { await api("restore_item", { id: item.id }); closeDetails(); await refresh(true); });
    });
  }

  async function purge(item: Item) {
    await run(async () => {
      const purgePrompt = locale.language === "nl"
        ? `Deze reservekopie definitief verwijderen: ${item.originalName}?\n\nDit kan het LAATSTE exacte origineel zijn, zeker als het bronbestand al is opgeruimd. De foto die je hebt bewaard, is geen exacte kopie hiervan. Dit kan niet ongedaan worden gemaakt.`
        : `Permanently delete this spare copy: ${item.originalName}?\n\nIt may be the LAST exact original, especially if its source file was cleaned up. The picture you kept is not an exact copy of it. This cannot be undone.`;
      if (!await confirm(purgePrompt, { title: t("Delete spare copy permanently"), kind: "warning" })) return;
      await mutate(async () => {
        await api("purge_item", { id: item.id, acknowledgeOriginalLoss: true });
        closeDetails();
        await refresh(true);
      });
    });
  }

  async function saveDate() {
    await run(async () => {
      if (!selected) return;
      const id = selected.item.id;
      await mutate(async () => {
        await api("set_date", { id, date: dateEntry });
        closeDetails();
        await refresh(true);
      });
    });
  }

  async function manageXmp(remove: boolean) {
    await run(async () => {
      if (!snapshot.root) return;
      if (remove && !await confirm(t("Remove unchanged sidecars created by Singulo? Existing or modified sidecars will be kept."), { title: t("Remove exported XMP"), kind: "warning" })) return;
      xmpResult = null;
      const root = snapshot.root;
      const summary = await api<XmpSummary>(remove ? "remove_exported_xmp" : "export_xmp");
      xmpResult = { root, summary };
    });
  }

  function closeDetails() {
    const id = selected && !comparing ? selected.item.id : null;
    detailSeq += 1;
    opening = false;
    selected = null; comparing = null; currentReview = null; zoom = null; resetVideos();
    if (!id) return;
    // Return to the last viewed photo in the grid, so keyboard and scroll position follow the lightbox.
    void tick().then(() => {
      const card = document.querySelector<HTMLElement>(`[data-id="${CSS.escape(id)}"]`);
      card?.scrollIntoView({ block: "nearest" });
      card?.focus({ preventScroll: true });
    });
  }

  $effect(() => {
    const next = { ...config };
    if (!native) return;
    const timer = setTimeout(() => {
      api<string[]>("path_examples", { settings: next })
        .then((value) => { examples = value; configError = ""; })
        .catch((reason: unknown) => { examples = []; configError = message(reason); });
    }, 180);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    const term = search;
    const selectedFilter = filter;
    const key = `${term}\0${selectedFilter}`;
    if (!loaded || !native || !hasArchive) return;
    if (key === lastQuery) { libraryLoading = false; return; }
    const seq = ++searchSeq;
    libraryLoading = true;
    const timer = setTimeout(() => {
      if (seq !== searchSeq) return;
      void loadLibrary(false, seq)
        .then(() => { if (seq === searchSeq) window.scrollTo({ top: 0 }); })
        .catch((reason: unknown) => {
          if (seq !== searchSeq) return;
          items = []; itemsStart = 0; months = []; month = ""; lastQuery = "";
          libraryLoading = false;
          error = message(reason);
        });
    }, 220);
    return () => { clearTimeout(timer); searchSeq += 1; };
  });

  onMount(() => {
    locale.language = window.localStorage.getItem("singulo.language") === "nl" ? "nl" : "en";
    thumbSize = Number(window.localStorage.getItem("singulo.thumbSize")) || 200;
    showInfo = window.localStorage.getItem("singulo.showInfo") !== "false";
    revealLabel = /Mac/i.test(navigator.userAgent) ? "Show in Finder" : /Windows/i.test(navigator.userAgent) ? "Show in Explorer" : "Show in folder";
    native = isTauri();
    mode = native ? getCurrentWindow().label : new URLSearchParams(window.location.search).get("window") ?? "main";
    if (!native) return;
    void getVersion().then((version) => { appVersion = version; }).catch((reason: unknown) => { console.error(reason); });
    const removers: UnlistenFn[] = [];
    let disposed = false;
    const register = (unsubscribe: UnlistenFn) => { if (disposed) unsubscribe(); else removers.push(unsubscribe); };
    void run(async () => {
      const current = getCurrentWindow();
      if (mode !== "viewer") {
        register(await current.onDragDropEvent((event) => {
          if (event.payload.type === "over") dropping = true;
          else if (event.payload.type === "leave") dropping = false;
          else if (event.payload.type === "drop") { dropping = false; void importFolders(event.payload.paths); }
        }));
      }
      register(await listen<Progress>("progress", (event) => {
        progressRevision += 1;
        snapshot.progress = event.payload;
        refreshImportedPhotos(event.payload);
      }));
      if (mode === "main") {
        register(await listen("imports-changed", () => {
          void refreshJobs().catch((reason: unknown) => { error = message(reason); });
        }));
      }
      register(await listen("library-changed", () => {
        if (mutating) return;
        void refresh(true).catch((reason: unknown) => { error = message(reason); });
      }));
      register(await listen<{ errors: string[] }>("xmp-attention", (event) => {
        xmpAttention = [...xmpAttention, ...event.payload.errors];
      }));
      register(await listen<string>("quit-blocked", (event) => { error = event.payload; }));
      register(await listen("closing", () => { notice = t("Stopping the import safely before closing. Retry the folders later to continue; copied files are kept."); }));
      await readSnapshot();
      if (!snapshot.root && mode === "main") {
        const path = await api<string | null>("last_archive");
        if (path) {
          try {
            await mutate(async () => { snapshot = await api<Snapshot>("open_archive", { path }); });
          } catch (reason: unknown) {
            notice = `${t("Previous archive unavailable")} (${message(reason)}). ${t("Connect the disk or choose a folder.")}`;
          }
        }
      }
      config = { ...snapshot.settings };
      if (!snapshot.root && mode === "main") tab = "import";
      if (mode === "main") tools = await api<ToolStatus[]>("tool_status");
      await refresh();
      loaded = true;
    });
    return () => { disposed = true; clearTimeout(undoTimer); clearTimeout(importLibraryTimer); for (const remove of removers) remove(); };
  });
</script>

<svelte:head><title>Singulo</title><meta name="description" content="An offline archive for pictures and videos" /></svelte:head>
<svelte:window onkeydown={onKey} onstorage={syncPreferences} />

{#snippet libraryView(viewer: boolean)}
  <div class="library-toolbar">
    <div class="search-field">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="m20 20-4-4" /></svg>
      <input class="search" aria-label={t("Search pictures and original folders")} bind:value={search} placeholder={t("Search pictures, dates or original folders...")} />
      {#if search}<button class="clear-search" aria-label={t("Clear search")} onclick={() => search = ""}>×</button>{/if}
    </div>
    <select aria-label={t("Filter library")} bind:value={filter}>
      <option value="all">{t("All pictures and videos")}</option><option value="video">{t("Videos")}</option><option value="unknown">{t("Unknown dates")}</option><option value="estimated">{t("Estimated dates")}</option>
      {#if !viewer}<option value="undo">{t("Spare copies of removed duplicates")}</option>{/if}
    </select>
    <label class="size-slider" title={t("Thumbnail size")}>
      <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="4" y="4" width="16" height="16" rx="3" /></svg>
      <input type="range" min="120" max="320" step="20" aria-label={t("Thumbnail size")} bind:value={thumbSize} onchange={updateThumbSize} />
      <svg width="19" height="19" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="2" y="2" width="20" height="20" rx="3" /></svg>
    </label>
  </div>
  {#if months.length}
    <div class="library-layout">
      <nav class="timeline" aria-label={t("Timeline")}>
        {#each years as entry (entry.year)}
          <button class="timeline-year" class:active={entry.year === year} onclick={() => {
            const first = months.find((candidate) => entry.year === "unknown" ? candidate.month === "unknown" : candidate.month.startsWith(`${entry.year}-`));
            if (first && first.month !== month) void changeMonth(first.month).catch((reason: unknown) => { error = message(reason); });
          }}><span>{entry.year === "unknown" ? t("Unknown date") : entry.year}</span><small>{entry.count.toLocaleString()}</small></button>
          {#if entry.year === year && year !== "unknown"}
            <div class="timeline-months">
              {#each months.filter((candidate) => candidate.month.startsWith(`${year}-`)) as entry (entry.month)}
                <button class:active={entry.month === month} aria-current={entry.month === month ? "true" : undefined} onclick={() => { if (entry.month !== month) void changeMonth(entry.month).catch((reason: unknown) => { error = message(reason); }); }}><span>{monthLabel(entry.month, "short")}</span><small>{entry.count.toLocaleString()}</small></button>
              {/each}
            </div>
          {/if}
        {/each}
      </nav>
      <section class="library-main" aria-busy={libraryLoading}>
        <div class="month-header">
          <div><h2>{monthLabel(month)}</h2><span>{monthCount.toLocaleString()} {t("pictures and videos")}{filtered ? ` · ${t("filtered")}` : ""}</span></div>
          <div class="button-row">
            <button class="secondary" disabled={monthPosition <= 0} onclick={() => changeMonth(months[monthPosition - 1].month).catch((reason: unknown) => { error = message(reason); })}>← {t("Newer")}</button>
            <button class="secondary" disabled={monthPosition < 0 || monthPosition >= months.length - 1} onclick={() => changeMonth(months[monthPosition + 1].month).catch((reason: unknown) => { error = message(reason); })}>{t("Older")} →</button>
          </div>
        </div>
        {#if itemsStart > 0}<button class="text-button load-earlier" disabled={loadingMore} onclick={loadEarlier}>{t("Show more recent pictures from this month")}</button>{/if}
        <div class="photo-grid" class:loading={libraryLoading} style={`--thumb:${thumbSize}px`}>
          {#each items as item (item.id)}
            <button class="photo-card" data-id={item.id} title={item.originalName} onclick={() => openItem(item.id)}>
              <Preview {item} />
              <span class="photo-caption"><strong>{item.originalName}</strong><small>{item.media.date?.slice(0, 10) ?? t("Date unknown")}{item.media.dateSource.startsWith("estimated") ? ` · ${t("estimated")}` : ""}</small></span>
              {#if item.state === "undo"}<span class="card-badge">{t("Spare copy")}</span>{/if}
            </button>
          {/each}
        </div>
        {#if itemsStart + items.length < monthCount}
          {#key items.length}<div class="load-more" {@attach loadOnScroll}>{loadingMore ? t("Loading more pictures...") : ""}</div>{/key}
        {/if}
      </section>
    </div>
  {:else if libraryLoading}
    <div class="empty"><p>{t("Loading your pictures...")}</p></div>
  {:else if !snapshot.root}
    <div class="empty"><h2>{viewer ? t("Open your archive in the main app.") : t("Start with an archive folder.")}</h2><p>{t("Singulo copies your photos and videos into this folder and keeps them organized there. Use a dedicated folder, ideally on an external disk with plenty of space.")}</p>{#if !viewer}<button class="primary" disabled={!native} onclick={chooseArchive}>{t("Choose folder")}</button>{/if}</div>
  {:else if filtered}
    <div class="empty"><h2>{search ? `${t("No matches for")} “${search}”` : t("No pictures match this filter.")}</h2><p>{t("Try another filename, date or original folder.")}</p><button class="secondary" onclick={clearSearch}>{t("Clear search")}</button></div>
  {:else}
    <div class="empty"><h2>{t("Your pictures will appear here.")}</h2><p>{viewer ? t("Imported pictures and videos will appear here.") : t("Add folders to fill your library. Originals stay untouched.")}</p>{#if !viewer}<button class="primary" disabled={!native || cancelling} onclick={() => importFolders()}>{t("Add folders +")}</button>{/if}</div>
  {/if}
{/snippet}

{#snippet archiveActivity(showImportLink = false)}
  {#if busy}
    <section class="banner archive-activity">
      <div class="activity-copy" role="status">
        <strong><span class="activity-indicator" class:paused={snapshot.progress.status === "paused"} aria-hidden="true"></span>{activityTitle}</strong>
        <p>{busyReason}</p>
        {#if importing}
          {#if tab !== "import"}<p><strong>{phaseTitle}</strong> {phaseDetail}</p>{/if}
          <p>{t(cancelling ? "You can still browse photos and switch tabs." : "You can still browse photos, switch tabs and queue more folders.")}</p>
          {#if snapshot.progress.status === "paused"}<p><strong>{t("Resume or cancel the import to make these actions available again.")}</strong></p>{/if}
        {/if}
      </div>
      {#if importing}
        <div class="activity-actions">
          {#if determinate}<strong>{percent}% · {snapshot.progress.done.toLocaleString()} / {snapshot.progress.total.toLocaleString()} {t("files")}</strong>{/if}
          {#if showImportLink && tab !== "import"}<button class="secondary" onclick={() => tab = "import"}>{t("View import progress")}</button>{/if}
        </div>
      {/if}
    </section>
  {/if}
{/snippet}

{#snippet importProgress()}
  <div class="progress-track" class:indeterminate={importing && !determinate} class:paused={snapshot.progress.status === "paused"} role="progressbar" aria-label={t("Import progress")} aria-valuemin={0} aria-valuemax={100} aria-valuenow={determinate ? percent : undefined}>
    <div style={`width:${determinate ? percent : 35}%`}></div>
  </div>
{/snippet}

{#snippet diskIcon(size: number)}
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><rect x="3" y="6" width="18" height="13" rx="3" /><path d="M3 14h18M7 17h1m3 0h1" /></svg>
{/snippet}

{#snippet logo(size: number)}
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" aria-hidden="true">
    <g stroke="currentColor" stroke-opacity=".55" stroke-width="1">
      <rect x="7" y="5.5" width="10" height="13" rx="1.5" transform="rotate(-14 12 12.5)" />
      <rect x="7" y="5.5" width="10" height="13" rx="1.5" transform="rotate(13 12 12.5)" />
    </g>
    <rect x="7" y="5.5" width="10" height="13" rx="1.5" fill="currentColor" />
    <rect x="8.2" y="6.7" width="7.6" height="7" rx=".7" fill="#274b38" />
    <path d="m8.2 12.8 2.4-2.7 1.7 1.9 1.3-1.4 2.2 2.4v.7H8.2Z" fill="currentColor" fill-opacity=".75" />
    <path d="M18.8 1.6c.3 1.3.8 1.8 2.1 2.1-1.3.3-1.8.8-2.1 2.1-.3-1.3-.8-1.8-2.1-2.1 1.3-.3 1.8-.8 2.1-2.1Z" fill="#e8c368" />
  </svg>
{/snippet}

{#if mode === "drop"}
  <main class="mini" class:dropping>
    <div class="mini-heading"><span class="brand-symbol">{@render logo(25)}</span><strong>Singulo</strong></div>
    <button class="mini-drop" disabled={!native || cancelling} onclick={() => snapshot.root ? importFolders() : chooseArchive()}>
      <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M12 3v12m-5-5 5 5 5-5" /><path d="M4 15v5h16v-5" /></svg>
      <strong>{dropping ? t("Drop to copy safely") : t("Drop photo folders here")}</strong>
      <small>{snapshot.root ? snapshot.root.split(/[\\/]/).pop() : t("Choose an archive first")}</small>
    </button>
    <div class="mini-bottom"><small>{busy ? `${activityTitle}${importing && determinate ? ` ${percent}%` : ""}` : t("Originals kept safe")}</small><button class="text-button" onclick={() => run(async () => { await api("show_window", { kind: "main" }); })}>{t("Open app")}</button></div>
    <label class="mini-pin"><input type="checkbox" bind:checked={onTop} onchange={() => run(async () => { await getCurrentWindow().setAlwaysOnTop(onTop); })} /> {t("Always on top")}</label>
    {#if error}<p class="mini-error">{error}</p>{:else if notice}<p class="mini-error mini-notice" role="status">{notice}</p>{/if}
  </main>
{:else if mode === "viewer"}
  <main class="viewer-shell">
    <header class="viewer-header">
      <div><strong>{t("Singulo Viewer")}</strong><small>{snapshot.root?.split(/[\\/]/).pop() ?? t("No archive open")}</small></div>
      <button class="secondary" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "main" }); })}>{t("Open main app")}</button>
    </header>
    {#if error && !selected}<div class="banner error" role="alert"><span>{error}</span><button class="text-button" onclick={() => error = ""}>{t("Dismiss")}</button></div>{/if}
    {#if notice}<div class="banner success" role="status">{notice}</div>{/if}
    {#if !native}<div class="banner warning">{t("Photo previews are available in the desktop app.")}</div>{/if}
    {@render libraryView(true)}
  </main>
{:else}
  <div class="app-shell">
    <aside class="sidebar">
      <div class="brand"><span class="brand-symbol">{@render logo(34)}</span><div><strong>Singulo</strong><small>{t("Pictures and videos.")}</small></div></div>
      <button class="primary full add-folders" disabled={!native || cancelling} onclick={() => importFolders()}>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>{t("Add folders")}
      </button>
      <p class="drop-hint">{t("Or drop folders anywhere in this window.")}</p>
      {#if importing}
        <button class="sidebar-progress" title={t("View import progress")} onclick={() => tab = "import"}>
          <span><strong>{t(snapshot.progress.status)}</strong>{#if determinate}<small>{percent}%</small>{/if}</span>
          {@render importProgress()}
        </button>
      {/if}
      <nav aria-label={t("Main navigation")}>
        <button class:active={tab === "library"} aria-current={tab === "library" ? "page" : undefined} onclick={() => tab = "library"}>
          <svg class="nav-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="3" y="3" width="7" height="7" rx="1.5" /><rect x="14" y="3" width="7" height="7" rx="1.5" /><rect x="3" y="14" width="7" height="7" rx="1.5" /><rect x="14" y="14" width="7" height="7" rx="1.5" /></svg>
          {t("Photos")} {#if snapshot.activeCount}<span class="nav-count">{snapshot.activeCount.toLocaleString()}</span>{/if}
        </button>
        <button class:active={tab === "review"} aria-current={tab === "review" ? "page" : undefined} onclick={() => tab = "review"}>
          <svg class="nav-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="2" y="5" width="9" height="14" rx="1.5" /><rect x="13" y="5" width="9" height="14" rx="1.5" /></svg>
          {t("Similarity review")} {#if reviews.length}<span class="nav-count attention">{reviews.length}</span>{/if}
        </button>
        <button class:active={tab === "import"} aria-current={tab === "import" ? "page" : undefined} onclick={() => tab = "import"}>
          <svg class="nav-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M12 3v11m-4-4 4 4 4-4" /><path d="M4 14v6h16v-6" /></svg>
          {t("Import & archive")} {#if importing}<span class="nav-count">{determinate ? `${percent}%` : t(snapshot.progress.status)}</span>{:else if incompleteJobs.length || snapshot.progress.errors.length || snapshot.progress.warnings?.length}<span class="nav-count attention">{incompleteJobs.length || "!"}</span>{/if}
        </button>
        <button class:active={tab === "settings"} aria-current={tab === "settings" ? "page" : undefined} onclick={() => tab = "settings"}>
          <svg class="nav-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0" /><circle cx="16" cy="6" r="2" /><circle cx="10" cy="12" r="2" /><circle cx="18" cy="18" r="2" /></svg>
          {t("Settings")} {#if settingsDirty}<span class="nav-count attention" title={t("Unsaved changes")}>•</span>{/if}
        </button>
      </nav>
      <div class="sidebar-bottom">
        <div class="display-controls">
          <div class="text-size" role="group" aria-label={t("Text size")}>
            <button title={t("Smaller text")} aria-label={t("Smaller text")} disabled={textScale === textScales[0]} onclick={() => stepTextScale(-1)}>A−</button>
            <button title={t("Reset text size")} aria-label={t("Reset text size")} onclick={() => setTextScale(1)}>{Math.round(textScale * 100)}%</button>
            <button title={t("Larger text")} aria-label={t("Larger text")} disabled={textScale === textScales[textScales.length - 1]} onclick={() => stepTextScale(1)}>A+</button>
          </div>
          <button class="contrast-toggle" aria-pressed={highContrast} onclick={toggleContrast}>
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M12 3a9 9 0 0 1 0 18Z" fill="currentColor" /></svg>{t("High contrast")}
          </button>
        </div>
        <button class="text-button full" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "drop" }); })}>{t("Open floating drop window")}</button>
        <button class="text-button full" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "viewer" }); })}>{t("Open separate viewer")}</button>
        <small class="version">SINGULO{appVersion ? ` / ${appVersion}` : ""}</small>
      </div>
    </aside>

    <main class="content">
      <header class="topbar"><span>{tab === "import" ? t("Your collection") : tab === "library" ? t("Browse your pictures") : tab === "review" ? t("Review similar pictures") : t("Archive settings")}</span><div class="topbar-right"><div class="status"><span class="safe-dot"></span>{busy ? activityTitle : t("One place for your pictures")}</div>{#if snapshot.root}<button class="archive-chip" title={`${t("Archive folder")}: ${snapshot.root}`} onclick={() => tab = "import"}>{@render diskIcon(16)}<span>{archiveLabel(snapshot.root)}</span></button>{:else}<button class="archive-chip missing" title={busyReason} disabled={!native || busy} onclick={chooseArchive}>{@render diskIcon(16)}<span>{t("Choose archive folder")}</span></button>{/if}</div></header>
      {@render archiveActivity(true)}
      {#if !native}<div class="banner warning"><strong>{t("Desktop preview")}</strong> {t("Folder imports and photo previews work in the Tauri desktop app. Run")} <code>npm run desktop</code> {t("to use them.")}</div>{/if}
      {#if error && !selected}<div class="banner error" role="alert"><span>{error}</span><button class="text-button" onclick={() => error = ""}>{t("Dismiss")}</button></div>{/if}
      {#if notice}<div class="banner success" role="status">{notice}</div>{/if}
      {#if incompleteJobs.length && tab !== "import"}
        <div class="banner warning incomplete-notice" role="status">
          <div><strong>{incompleteJobs.length} {t("import(s) need attention")}</strong><p>{t("Some source folders may not be fully backed up. Review the incomplete imports before cleaning up sources.")}</p></div>
          <button class="secondary" onclick={() => tab = "import"}>{t("Review incomplete imports")}</button>
        </div>
      {/if}
      {#if snapshot.progress.errors.length}<details class="banner error"><summary>{snapshot.progress.errors.length} {t("import/recovery issue(s) need attention")}</summary>{#each snapshot.progress.errors as issue}<p>{issue}</p>{/each}</details>{/if}
      {#if snapshot.progress.warnings?.length}<details class="banner warning"><summary>{snapshot.progress.warnings.length} {t("XMP sidecar warning(s)")}</summary>{#each snapshot.progress.warnings as issue}<p>{issue}</p>{/each}</details>{/if}
      {#if xmpAttention.length}<details class="banner warning"><summary>{xmpAttention.length} {t("XMP sidecar warning(s)")}</summary>{#each xmpAttention as issue}<p>{issue}</p>{/each}<button class="text-button" onclick={() => xmpAttention = []}>{t("Dismiss")}</button></details>{/if}

      {#if tab === "import"}
        <div class="page-heading plain"><h1>{t("Import & archive")}</h1></div>
        <section class="destination">
          <span class="disk-icon">{@render diskIcon(20)}</span>
          <div><small>{t("ARCHIVE DESTINATION")}</small><strong>{snapshot.root ?? t("No archive open")}</strong>{#if !snapshot.root}<p>{t("Singulo copies your photos and videos into this folder and keeps them organized there. Use a dedicated folder, ideally on an external disk with plenty of space.")}</p>{/if}</div>
          <button class={snapshot.root ? "secondary" : "primary"} title={busyReason} disabled={!native || busy} onclick={chooseArchive}>{snapshot.root ? t("Change") : t("Choose folder")}</button>
        </section>
        {#if incompleteJobs.length}
          <section class="history incomplete-imports">
            <h3>{t("Imports needing attention")}</h3>
            <p class="muted">{t("Retrying skips files that are already safely copied.")}</p>
            {#each incompleteJobs as job (job.id)}
              {@const reason = retryReason(job)}
              <article class="incomplete-import">
                <div class="section-line"><strong>{t(jobStatus(job))}</strong><div class="review-actions"><button class="secondary" title={reason} disabled={!!reason} onclick={() => importFolders(job.roots)}>{t("Retry folders")}</button><button class="text-button" title={busyReason} disabled={busy} onclick={() => startClearImport(job)}>{t("Clear import...")}</button></div></div>
                <p>{jobExplanation(job)}</p>
                {#if job.progress.folders?.length}
                  {#each job.progress.folders as folder (folder.root)}
                    {@const incomplete = !folder.scanComplete || folder.processed < folder.discovered || folder.failed > 0 || folder.scanErrors > 0}
                    <div class="import-folder">
                      <strong class:field-error={incomplete}>{t(incomplete ? "Incomplete" : "Imported")}</strong>
                      <code>{folder.root}</code>
                      <small>{folder.copied} {t("copied")} / {folder.duplicates} {t("exact duplicates")} / {folder.processed} {t("processed out of")} {folder.discovered} {t("recognized media files found")}</small>
                      {#if !folder.scanComplete}<p class="field-error">{t("Folder scan did not finish. The total number of source files is not yet known.")}</p>{/if}
                      {#if folder.processed < folder.discovered}<p class="field-error">{folder.discovered - folder.processed} {t("file(s) were not reached.")}</p>{/if}
                      {#if folder.failed || folder.scanErrors}<p class="field-error">{folder.failed} {t("file error(s)")} / {folder.scanErrors} {t("folder scan error(s). See the issues below.")}</p>{/if}
                    </div>
                  {/each}
                {:else}
                  {#each job.roots as root}<code class="import-root">{root}</code>{/each}
                  <p class="muted">{t("No verified folder totals are available. Retry to check every source folder.")}</p>
                {/if}
                {#if job.progress.errors?.length}
                  <details class="import-issues"><summary>{job.progress.errors.length} {t("issue(s): why files may be missing")}</summary>{#each job.progress.errors as issue}<p>{issue}</p>{/each}</details>
                {/if}
                {#if job.progress.warnings?.length}
                  <details class="import-issues"><summary>{job.progress.warnings.length} {t("XMP sidecar warning(s)")}</summary>{#each job.progress.warnings as issue}<p>{issue}</p>{/each}</details>
                {/if}
              </article>
            {/each}
          </section>
        {/if}
        {#if busy || snapshot.progress.total > 0}
          <section class="progress-card"><div class="section-line"><strong>{busy ? activityTitle : t(snapshot.progress.status)}</strong>{#if determinate}<span>{percent}% · {snapshot.progress.done} / {snapshot.progress.total} {t("files")}</span>{/if}</div>{@render importProgress()}
            {#if importing}
              <div class="import-stage">
                <strong>{phaseTitle}</strong>
                <p class="stage-detail">
                  {phaseDetail || (displayedStage.done === 0 && displayedStage.status === "importing" && displayedStage.phase !== "finalizing" ? t("Preparing the first files. The percentage starts when files have been processed.") : "")}
                  {#if displayedStage.status === "importing" && ["hashing", "metadata"].includes(displayedStage.phase ?? "")}
                    <span>{t("Cloud-only originals may need to download first. External drives can also take longer to read.")}</span>
                  {/if}
                </p>
              </div>
            {/if}
            {#if importing}<p class="current-file" title={displayedStage.current || undefined}>{displayedStage.current}</p>{/if}
            <p class="muted">{snapshot.progress.copied} {t("originals copied")} / {snapshot.progress.duplicates} {t("exact duplicates skipped")} · {t("Total elapsed")}: {(elapsedMs / 1000).toFixed(0)} s</p>
            {#if snapshot.progress.timings}
              {@const timings = snapshot.progress.timings}
              <details class="advanced"><summary>{t("Import timings")}</summary>
                <p>{t("scanning")}: {(timings.scanMs / 1000).toFixed(1)} s</p>
                <p>{t("Reading and hashing")}: {(timings.hashMs / 1000).toFixed(1)} s / {t("Metadata")}: {(timings.metadataMs / 1000).toFixed(1)} s / {t("Buffering")}: {(timings.bufferMs / 1000).toFixed(1)} s</p>
                <p>{t("Decoding (combined worker time)")}: {(timings.decodeMs / 1000).toFixed(1)} s / {t("Matching")}: {(timings.matchMs / 1000).toFixed(1)} s</p>
                <p>{t("Copying and catalog writes")}: {(timings.placementMs / 1000).toFixed(1)} s / {t("Recovery backup")}: {(timings.backupMs / 1000).toFixed(1)} s / {t("Total elapsed")}: {(elapsedMs / 1000).toFixed(1)} s</p>
                <p>{t("Stage times overlap while decoding runs in parallel; hashing includes exact-match verification.")}</p>
              </details>
            {/if}
            {#if importing}<div class="button-row"><button class="secondary" disabled={cancelling} onclick={() => run(async () => { await api("control_import", { action: snapshot.progress.status === "paused" ? "resume" : "pause" }); })}>{snapshot.progress.status === "paused" ? t("Resume") : t("Pause")}</button><button class="text-button" disabled={cancelling} onclick={() => run(async () => { await api("control_import", { action: "cancel" }); })}>{t("Cancel import")}</button></div>{/if}
          </section>
        {/if}
        {#if importing || queuedJobs.length}
          <section class="history import-queue">
            <h3>{t("Source folders and queue")}</h3>
            {#if importing}
              {#each snapshot.progress.folders ?? [] as folder (folder.root)}
                <div class="import-folder">
                  <strong>{t("Current import")}</strong><code>{folder.root}</code>
                  <small>{folder.processed.toLocaleString()} {t("processed out of")} {folder.discovered.toLocaleString()} {t("recognized media files found")} · {t(folder.scanComplete ? "Scan complete" : "Scan not finished")}</small>
                </div>
              {/each}
            {/if}
            {#if queuedJobs.length}
              <p class="muted">{t("Queued folders start after the current import. They have not been scanned yet.")}</p>
              {#each queuedJobs as job (job.id)}
                {#each job.roots as root}
                  <div class="import-folder"><strong>{t("Queued — waiting to scan")}</strong><code>{root}</code></div>
                {/each}
              {/each}
            {/if}
          </section>
        {/if}
        <div class="stats"><div class="stat"><small>{t("PICTURES AND VIDEOS")}</small><strong>{snapshot.activeCount.toLocaleString()}</strong></div><div class="stat"><small>{t("ARCHIVE SIZE")}</small><strong>{bytes(snapshot.activeBytes)}</strong></div><div class="stat" title={t("Replaced lower-quality copies, kept so a duplicate merge can be undone.")}><small>{t("SPARE COPIES")}</small><strong>{snapshot.undoCount.toLocaleString()}</strong><span>{bytes(snapshot.undoBytes)}</span></div></div>
        <section class="next-steps"><div><h3>{t("Clean up source folders")}</h3><p>{t("Moves source files with an identical archived copy to the system Trash.")}</p></div><button class="secondary" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={startCleanup}>{t("Review source cleanup")}</button></section>
        {#if jobs.some((job) => job.progress.status === "complete")}<section class="history"><h3>{t("Recent completed imports")}</h3>{#each jobs.filter((job) => job.progress.status === "complete").slice(0, 5) as job (job.id)}<div class="history-row"><div><strong>{job.roots.map((path) => path.split(/[\\/]/).pop()).join(", ")}</strong><small>{t("complete")} / {job.progress.copied ?? 0} {t("copied")} / {job.progress.duplicates ?? 0} {t("exact duplicates")}</small>{#if job.progress.warnings?.length}<details class="import-issues"><summary>{job.progress.warnings.length} {t("XMP sidecar warning(s)")}</summary>{#each job.progress.warnings as issue}<p>{issue}</p>{/each}</details>{/if}</div></div>{/each}</section>{/if}
      {:else if tab === "library"}
        <div class="library-page">{@render libraryView(false)}</div>
      {:else if tab === "review"}
        <div class="page-heading"><div><p class="eyebrow">{t("SIMILAR DOESN'T ALWAYS MEAN THE SAME")}</p><h1>{t("A second look.")}</h1><p class="subtitle">{t("Compare uncertain matches. Edits, bursts and RAW/JPEG pairs deserve to stay.")}</p></div><div class="review-actions"><span class="pill">{reviews.length} {t("to review")}</span>{#if reviews.length}<span class="disabled-hint" title={reviewDisabledReason}><button class="primary" disabled={busy || opening} onclick={() => openReview(0)}>{t("Start reviewing")}</button></span>{/if}</div></div>
        {#if reviews.length}
          <div class="review-list">
            {#each reviews as review, index (`${review.item}:${review.candidate}`)}
              <div class="disabled-hint" title={reviewDisabledReason}>
                <button class="review-row" aria-label={`${t("Compare similar pictures")}: ${review.reason}`} disabled={busy || opening} onclick={() => openReview(index)}>
                  <span class="review-thumbnails"><Preview item={review.itemPreview} /><Preview item={review.candidatePreview} /></span>
                  <span class="review-description"><strong>{t("Two similar pictures")}</strong><span>{review.reason}</span></span>
                  <span class="secondary">{t("Compare files")}</span>
                </button>
              </div>
            {/each}
          </div>
        {:else}<div class="empty"><span class="empty-check" aria-hidden="true">OK</span><h2>{t("Nothing needs a second look.")}</h2><p>{t("Uncertain matches will appear here. Exact duplicates are handled automatically.")}</p></div>{/if}
      {:else}
        <div class="page-heading"><div><p class="eyebrow">{t("SIMPLE SETTINGS, YOUR WAY")}</p><h1>{t("A place for everything.")}</h1><p class="subtitle">{t("Archive settings travel with your disk. Changes apply to future imports.")}</p></div></div>
        <section class="settings-card"><h3>{t("Language")}</h3><label class="language-setting">{t("Choose the interface language")}<select aria-label={t("Language")} value={locale.language} onchange={changeLanguage}><option value="en">English</option><option value="nl">Nederlands</option></select></label></section>
        <section class="settings-card"><h3>{t("Folder organization")}</h3><label>{t("Choose a layout")}<select bind:value={config.template}>{#each presets as [value, label]}<option {value}>{t(label)}</option>{/each}{#if !presets.some(([value]) => value === config.template)}<option value={config.template}>{t("Custom layout")}</option>{/if}</select></label><details class="advanced"><summary>{t("Advanced folder template")}</summary><label>{t("Template")}<input bind:value={config.template} placeholder={"Family/{yyyy}/{mm}"} /></label><p>{t("Supported:")} <code>{"{yyyy}"}</code> <code>{"{mm}"}</code> <code>{"{dd}"}</code> <code>{"{media_type}"}</code>. {t("Safe literal folder names are welcome.")}</p></details>
          <div class="setting-columns"><label>{t("Filenames")}<select bind:value={config.filenames}><option value="original">{t("Keep original filenames")}</option><option value="date">{t("Use photo date / time")}</option></select></label><label>{t("Unknown-date folder")}<input bind:value={config.unknownFolder} /></label></div>
          <div class="path-preview"><small>{t("YOUR FILES WOULD LOOK LIKE THIS")}</small>{#each examples as example}<code>{example}</code>{/each}{#if !native}<code>2023/07/18/IMG_1234.jpg</code><code>unknown/undated.jpg</code>{/if}{#if configError}<p class="field-error">{configError}</p>{/if}<p>{t("Name collisions receive a unique suffix. Estimated dates stay marked; missing times aren't invented.")}</p></div>
          <label class="check-setting"><input type="checkbox" bind:checked={config.automaticMatches} /><span><strong>{t("Automatically group very confident matches")}</strong><small>{t("The clearly better version stays in your library. The duplicate is removed, but a spare copy is kept so you can restore it. Doubtful cases are left for you to compare.")}</small></span></label>
          <label class="check-setting"><input type="checkbox" bind:checked={config.deepVideoMatching} disabled={!config.automaticMatches} /><span><strong>{t("Deep video comparison (slower)")}</strong><small>{t("Off by default: exact video duplicates are recognized by hash; possible matches from a preview or capture date and duration go to manual review. Enable to compare every frame and audio before automatically grouping a clearly higher-resolution equivalent. Requires automatic grouping above and can slow imports substantially.")}</small></span></label>
          <div class="button-row"><button class="primary" title={busyReason} disabled={!native || !snapshot.root || busy || !!configError || !settingsDirty} onclick={() => run(async () => { await mutate(async () => { await api("save_settings", { settings: config }); await refresh(true); }); notice = t("Settings saved for future imports. Existing files haven't moved."); })}>{t("Save archive settings")}</button>{#if settingsDirty}<span class="pill unsaved">{t("Unsaved changes")}</span><button class="text-button" onclick={() => config = { ...snapshot.settings }}>{t("Discard changes")}</button>{/if}<button class="text-button" onclick={() => config = { ...defaults }}>{t("Reset defaults")}</button></div>
        </section>
        <section class="settings-card"><h3>{t("Choosing an external disk")}</h3><p class="muted">{t("Mac only: prefer APFS. Windows only: prefer NTFS. For use on both, exFAT is convenient, but lacks journaling and may require extra copying during import. Always eject safely and keep another backup.")}</p><p class="muted">{t("You do not need to reformat an existing disk. Formatting erases its files; this app never formats disks.")}</p></section>
        <section class="next-steps"><div><h3>{t("Apply the saved layout to older imports?")}</h3><p>{t("Preview existing file moves first. Sources and folder history stay untouched.")}</p></div><button class="secondary" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={previewMoves}>{t("Preview reorganization")}</button></section>
        <button class="text-button" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={() => run(async () => { moves = await api<MoveRow[]>("rollback_preview"); undoingMoves = true; })}>{t("Preview undo of last reorganization")}</button>
        <section class="settings-card">
          <h3>{t("Correct estimated dates")}</h3>
          <p class="muted">{t("Find dates in original filenames and folders for existing photos and videos. Affected archive files move to match their corrected dates; source files stay untouched. Camera and manually corrected dates are kept.")}</p>
          <p class="muted">{t("Items with conflicting folder dates or shared sidecars are skipped for safety. Spare copies are not changed.")}</p>
          <button class="secondary" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={repairDates}>{t("Correct estimated dates")}</button>
        </section>
        <section class="settings-card">
          <h3>{t("Export metadata to XMP")}</h3>
          <p class="muted">{t("Share corrected dates and original folder names with apps such as Immich. Singulo keeps its sidecars up to date; originals and existing sidecars are not changed.")}</p>
          <p class="muted">{t("Existing sidecars are merged when safe. If the required filename is occupied, the photo is skipped and reported. Modified exports are kept.")}</p>
          <div class="button-row">
            <button class="secondary" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={() => manageXmp(false)}>{t("Export XMP")}</button>
            <button class="text-button" title={busyReason} disabled={!native || !snapshot.root || busy} onclick={() => manageXmp(true)}>{t("Remove exported XMP")}</button>
          </div>
          {#if xmpResult && xmpResult.root === snapshot.root}
            <p role="status">{t("Created")}: {xmpResult.summary.created} · {t("Updated")}: {xmpResult.summary.updated} · {t("Removed")}: {xmpResult.summary.removed} · {t("Skipped")}: {xmpResult.summary.skipped}</p>
            {#if xmpResult.summary.errors.length}<details><summary>{t("Sidecars needing attention")}: {xmpResult.summary.errors.length}</summary><ul>{#each xmpResult.summary.errors as issue}<li>{issue}</li>{/each}</ul></details>{/if}
          {/if}
        </section>
        <section class="settings-card"><h3>{t("Media tools")}</h3><p class="muted">{t("RAW and video formats depend on the bundled decoders. Unsupported originals can still be archived without a preview.")}</p><div class="tool-list">{#each tools as tool}<div><span class:unavailable={!tool.available} class="safe-dot"></span><strong>{tool.name}</strong><small title={tool.detail}>{tool.available ? t("Available") : tool.detail}</small></div>{/each}</div></section>
      {/if}
      <footer>Singulo <span>{t("Sources stay safe until you explicitly clean them up.")}</span></footer>
    </main>
  </div>
  {#if dropping}
    <div class="drop-overlay" aria-hidden="true"><div><svg width="44" height="44" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M3 8V5h7l2 3h9v12H3V8Z" /><path d="M12 10v7m-3-3 3 3 3-3" /></svg><strong>{snapshot.root ? t("Drop to copy into your archive") : t("Drop, then choose an archive folder")}</strong><small>{t("Originals stay untouched.")}</small></div></div>
  {/if}
{/if}

{#if selected && !comparing}
  {@const item = selected.item}
  <div class="lightbox" role="dialog" aria-modal="true" aria-label={item.originalName} tabindex="-1" {@attach focusOnMount}>
    <header class="lightbox-top">
      <button class="lightbox-button" onclick={closeDetails}>← {t("Back to thumbnails")}</button>
      <div class="lightbox-title"><strong>{item.originalName}</strong><small>{absoluteIndex >= 0 ? `${(absoluteIndex + 1).toLocaleString()} / ${monthCount.toLocaleString()} · ${monthLabel(month)}` : ""}</small></div>
      <div class="lightbox-tools">
        <button class="lightbox-button" class:active={showInfo} aria-pressed={showInfo} onclick={toggleInfo}>{t("Info")}</button>
        <button class="lightbox-button" aria-label={t("Close")} onclick={closeDetails}>×</button>
      </div>
    </header>
    {#if error}<div class="banner error lightbox-error" role="alert"><span>{error}</span><button class="text-button" onclick={() => error = ""}>{t("Dismiss")}</button></div>{/if}
    {#if mode !== "viewer"}{@render archiveActivity()}{/if}
    <div class="lightbox-body">
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="lightbox-stage" onclick={(event) => { const target = event.target as HTMLElement; if (target === event.currentTarget || target.classList.contains("preview")) closeDetails(); }}>
        <button class="lightbox-nav previous" disabled={!canPrevious || opening} aria-label={t("Previous")} onclick={() => navigatePhoto(-1)}>‹</button>
        {#if videos[0]?.source}
          {@const video = videos[0]}
          <video src={video.source} controls autoplay onerror={() => videoFailed(0, video.id, video.source)}><track kind="captions" /></video>
        {:else}
          <Preview {item} large />
          {#if item.media.kind === "video"}
            <div class="play-overlay">
              {#if videos[0]}<span role="status">{t(videos[0].phase === "converting" ? "Preparing a playable copy..." : "Opening video...")}</span>
              {:else}<button class="primary" onclick={() => playVideo(item)}>▶ {t("Play video")}</button>{/if}
            </div>
          {/if}
        {/if}
        <button class="lightbox-nav next" disabled={!canNext || opening} aria-label={t("Next")} onclick={() => navigatePhoto(1)}>›</button>
      </div>
      {#if showInfo}
        <aside class="lightbox-info">
          <dl>
            <dt>{t("Date")}</dt><dd>{formatDate(item.media.date)}<small>{dateSourceLabel(item.media.dateSource)}</small></dd>
            <dt>{t("Dimensions")}</dt><dd>{item.media.width} × {item.media.height} · {bytes(item.media.bytes)} · {item.media.kind.toUpperCase()}</dd>
            {#if item.albums.length}<dt>{t("Original folders")}</dt><dd>{item.albums.join(" · ")}</dd>{/if}
            <dt>{t("Location in archive")}</dt><dd><code>{item.path}</code></dd>
          </dl>
          <button class="secondary full" disabled={!native} onclick={() => reveal(item)}>{t(revealLabel)}</button>
          {#if selected.companions.length}<p class="banner warning">{t("This picture belongs with related files (RAW/JPEG, sidecar or Live Photo), so it can't be removed on its own as a duplicate.")}</p>{/if}
          {#if item.media.warnings.length}<details><summary>{t("Metadata & quality notes")}</summary>{#each item.media.warnings as warning}<p class="muted">{warning}</p>{/each}</details>{/if}
          <details><summary>{t("Original folders & import history")}</summary>{#each selected.sources as source}<p class="source-history"><strong>{source.album}</strong><code>{source.relative}</code><small>{source.status.startsWith("staged:") ? t("Cleanup recovery needed") : source.status === "trashed" ? t("Source moved to OS trash") : t("Source retained")}</small></p>{/each}</details>
          {#if mode !== "viewer"}
            <div class="lightbox-actions">
              {#if item.state === "undo"}
                <button class="primary full" title={busyReason} disabled={busy} onclick={() => restore(item)}>{t("Restore to library")}</button>
                <button class="danger-button full" title={busyReason} disabled={busy} onclick={() => purge(item)}>{t("Delete spare copy permanently")}</button>
              {:else}
                <label>{t("Correct the date")}<input type="date" bind:value={dateEntry} /></label>
                <button class="secondary full" title={busyReason} disabled={busy || !dateEntry || dateEntry === item.media.date?.slice(0, 10)} onclick={saveDate}>{t("Save date & reorganize this file")}</button>
              {/if}
            </div>
          {/if}
          <p class="key-hint">{t("← → browse · I info · Esc back")}</p>
        </aside>
      {/if}
    </div>
  </div>
{/if}

{#if selected && comparing && mode !== "viewer"}
  {@const pair = [selected, comparing]}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(event) => { if (!working && event.target === event.currentTarget) closeDetails(); }}>
    <div class="modal comparison" role="dialog" aria-modal="true" aria-label={t("Compare similar pictures")} tabindex="-1" {@attach focusOnMount}>
      <div class="modal-top">
        <div><small>{t("COMPARE THE ORIGINALS")} · {t("Match")} {reviewIndex + 1} {t("of")} {reviews.length}</small><h2>{t("Which copy would you keep?")}</h2></div>
        <div class="button-row compact">
          <button class="secondary" title={reviewDisabledReason} disabled={opening || busy || reviews.length < 2} onclick={() => stepReview(-1)}>← {t("Previous")}</button>
          <button class="secondary" title={reviewDisabledReason} disabled={opening || busy || reviews.length < 2} onclick={() => stepReview(1)}>{t("Skip for now")} <kbd>S</kbd></button>
          <button class="secondary" onclick={closeDetails}>{t("Close")}</button>
        </div>
      </div>
      {#if error}<div class="banner error" role="alert"><span>{error}</span><button class="text-button" onclick={() => error = ""}>{t("Dismiss")}</button></div>{/if}
      {@render archiveActivity()}
      {#if selected.companions.length && comparing.companions.length}
        <p class="banner warning">{t("Both pictures belong with related files and can't be removed on their own. Choose Keep both / dismiss match to finish this pair.")}</p>
      {/if}
      <div class="detail-columns">
        {#each pair as detail, index (detail.item.id)}
          {@const other = index === 0 ? comparing : selected}
          <div class="detail-column">
            <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
            <div class="compare-pane" class:zoomed={!!zoom} style={zoom ? `--zx:${zoom.x}%;--zy:${zoom.y}%` : ""} title={t("Click to zoom both pictures")} onclick={zoomAt} onmousemove={panZoom}>
              {#if videos[index]?.source}
                {@const video = videos[index]}
                {#if index === 0}<video bind:this={firstVideo} src={video.source} controls autoplay onerror={() => videoFailed(index, video.id, video.source)}><track kind="captions" /></video>
                {:else}<video bind:this={secondVideo} src={video.source} controls autoplay onerror={() => videoFailed(index, video.id, video.source)}><track kind="captions" /></video>{/if}
              {:else}
                <Preview item={detail.item} large />
              {/if}
              {#if pictureComparison?.recommendation === index}<span class="quality-label">{t("Probably better")}</span>{/if}
            </div>
            {#if detail.item.media.kind === "video" && !videos[index]?.source}
              {#if videos[index]}<p class="muted" role="status">{t(videos[index].phase === "converting" ? "Preparing a playable copy..." : "Opening video...")}</p>
              {:else}<button class="secondary full" disabled={working} onclick={() => playVideo(detail.item, index)}>▶ {t("Play video")}</button>{/if}
            {/if}
            <button class="primary keep-button" title={reviewDisabledReason ?? (other.companions.length ? t("Not possible: the other picture belongs with related files (RAW/JPEG, sidecar or Live Photo) and can't be removed on its own.") : undefined)} disabled={opening || busy || !!other.companions.length} onclick={() => resolve(detail.item.id)}>
              <span>{t("Keep this one")} <kbd>{index + 1}</kbd></span><small title={detail.item.originalName}>{detail.item.originalName}</small>
            </button>
            {#if other.companions.length && !detail.companions.length}<p class="muted">{t("Not possible: the other picture belongs with related files (RAW/JPEG, sidecar or Live Photo) and can't be removed on its own.")}</p>{/if}
          </div>
        {/each}
      </div>
      <button class="secondary comparison-dismiss" title={reviewDisabledReason} disabled={opening || busy} onclick={() => resolve(null)}>{t("Keep both / dismiss match")} <kbd>B</kbd></button>
      {#if videos[0]?.source && videos[1]?.source}<div class="button-row"><button class="secondary" onclick={() => run(async () => { if (firstVideo && secondVideo) { firstVideo.currentTime = 0; secondVideo.currentTime = 0; await Promise.all([firstVideo.play(), secondVideo.play()]); } })}>{t("Play both from start")}</button><button class="secondary" onclick={() => { firstVideo?.pause(); secondVideo?.pause(); }}>{t("Pause both")}</button></div>{/if}
      <table class="difference-table">
        <tbody>
          {#each [
            { label: t("Resolution"), values: pair.map((d) => `${d.item.media.width} × ${d.item.media.height}`), higher: pictureComparison?.resolution.substantiallyHigher, hint: t("at least 2x pixels") },
            { label: t("File size"), values: pair.map((d) => bytes(d.item.media.bytes)), higher: pictureComparison?.size.substantiallyHigher, hint: t("at least 2x size") },
            { label: t("Type"), values: pair.map((d) => `${d.item.media.kind.toUpperCase()} · ${d.item.originalName.split(".").pop()?.toUpperCase() ?? ""}`) },
            { label: t("Date"), values: pair.map((d) => formatDate(d.item.media.date)), notes: pair.map((d) => dateSourceLabel(d.item.media.dateSource)) },
            { label: t("Original folders"), values: pair.map((d) => d.item.albums.join(" · ") || "—") },
            { label: t("Related files"), values: pair.map((d) => d.companions.length ? t("Yes, kept together") : "—") },
            { label: t("Location in archive"), values: pair.map((d) => d.item.path), code: true },
          ] as row (row.label)}
            <tr class:differs={row.values[0] !== row.values[1]}>
              <th scope="row">{row.label}</th>
              {#each row.values as value, index}
                <td class:quality-higher={row.higher === index}>{#if row.code}<code>{value}</code>{:else}{value}{/if}{#if row.higher === index} <small>({row.hint})</small>{/if}{#if row.notes}<small class="cell-note">{row.notes[index]}</small>{/if}</td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
      {#if pictureComparison && pictureComparison.resolution.higher !== null && pictureComparison.size.higher !== null && pictureComparison.resolution.higher !== pictureComparison.size.higher}
        <p class="muted">{t("Resolution and file size favor different pictures, so neither is recommended.")}</p>
      {/if}
      <details>
        <summary>{t("Why were these matched?")}</summary>
        {#if currentReview?.reason}<p class="muted">{currentReview.reason}</p>{/if}
        <p class="muted">{t("Keeping one picture removes the other from your library. A spare copy is kept, so you can always restore it. Keep both / dismiss match leaves both in your library and clears this pair.")}</p>
        <p class="muted">{t("Highlighted measurements are at least twice as large. File size and pixel count do not guarantee better quality; compare the originals before choosing.")}</p>
        {#each pair as detail}
          <p class="source-history"><strong>{detail.item.originalName}</strong>{#each detail.sources as source}<code>{source.album} / {source.relative}</code>{/each}</p>
        {/each}
      </details>
      <p class="key-hint">{t("1 / 2 keep · B keep both · ← → previous / next match · Z zoom · Esc close")}</p>
    </div>
  </div>
{/if}

{#if cleanup !== null}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(event) => { if (!working && event.target === event.currentTarget) cleanup = null; }}>
    <div class="modal" role="dialog" aria-modal="true" aria-label={t("Source cleanup preview")} tabindex="-1" {@attach focusOnMount}>
      <div class="modal-top"><div><small>{t("EXPLICITLY VERIFIED, NEVER AUTOMATIC")}</small><h2>{t("Clean up source folders.")}</h2></div><button class="secondary" disabled={working} onclick={() => cleanup = null}>{t("Close")}</button></div>
      <div class="banner warning">{t("Only byte-identical archived originals qualify. Cleanup may leave one copy. Trash usually needs emptying before space is freed.")}</div>
      {#if incompleteJobs.length}<p class="banner warning">{t("Some imports are incomplete. Cleanup only includes verified copies, not every file in the selected folders. Unimported files stay in place.")}</p>{/if}
      {#if error || cleanupError}<div class="banner error" role="alert">{cleanupError || error}</div>{/if}
      {#if cleanupLoading}
        <div class="checking-files" role="status"><span class="activity-indicator" aria-hidden="true"></span><div><strong>{t("Checking source files...")}</strong><p>{t("Verifying originals against the archive. Large folders and external disks can take a while. You can close this window while checking continues.")}</p></div></div>
      {:else if cleanupError}
        <button class="secondary" disabled={busy} onclick={startCleanup}>{t("Try again")}</button>
      {:else}
        <p class="muted">{t("Select folders to clean up verified source files. Expand a folder below to choose individual files.")}</p>
        <div class="cleanup-list">
          {#each cleanupFolders as folder (folder.root)}
            {@const selectedCount = folder.eligible.reduce((count, id) => count + Number(cleanupSelected.has(id)), 0)}
            <section class="cleanup-folder">
              <label class="cleanup-row">
                <input type="checkbox" checked={folder.eligible.length > 0 && selectedCount === folder.eligible.length} indeterminate={selectedCount > 0 && selectedCount < folder.eligible.length} disabled={busy || !folder.eligible.length} onchange={(event) => toggleCleanupFolder(folder, event.currentTarget.checked)} />
                <span><strong>{folder.root}</strong><small>{selectedCount} / {folder.eligible.length} {t("eligible files selected")} · {folder.rows.length - folder.eligible.length} {t("file(s) will stay in place")}</small></span>
              </label>
              <details class="cleanup-file-options" ontoggle={(event) => { cleanupFileFolders = event.currentTarget.open ? [...cleanupFileFolders, folder.root] : cleanupFileFolders.filter((root) => root !== folder.root); }}>
                <summary>{t("Choose individual files / see why files are kept")}</summary>
                {#if cleanupFileFolders.includes(folder.root)}
                  {#each folder.rows.slice(0, cleanupFileLimits[folder.root] ?? 100) as row (row.id)}
                    <label class="cleanup-row"><input type="checkbox" bind:group={cleanupSelection} value={row.id} disabled={!row.eligible || busy} /><span><strong>{row.source}</strong><small class:field-error={!row.eligible}>{t(row.reason)}</small></span><small>{bytes(row.bytes)}</small></label>
                  {/each}
                  {#if folder.rows.length > (cleanupFileLimits[folder.root] ?? 100)}<button class="text-button" onclick={() => cleanupFileLimits[folder.root] = (cleanupFileLimits[folder.root] ?? 100) + 100}>{t("Show more files")}</button>{/if}
                {/if}
              </details>
            </section>
          {/each}
          {#if !cleanup.length}<p class="muted">{t("No pending imported sources to clean up.")}</p>{/if}
        </div>
        <label class="check-setting"><input type="checkbox" bind:checked={emptyFolders} disabled={busy} /><span><strong>{t("Also remove genuinely empty imported subfolders")}</strong><small>{t("No recursive deletion. Dropped roots, hidden files and nonempty folders remain.")}</small></span></label>
      {/if}
      {#if working}<p class="checking-files" role="status"><span class="activity-indicator" aria-hidden="true"></span>{t("Cleaning selected sources...")}</p>{/if}
      {#each cleanupResults as result}<p class="cleanup-result">{result}</p>{/each}
      <div class="modal-bottom button-row"><strong>{cleanupSelection.length} {t("selected")} / {bytes(cleanupBytes)} {t("to trash")}</strong><button class="primary" disabled={busy || !!cleanupError || !cleanupSelection.length} onclick={cleanSources}>{t("Confirm cleanup...")}</button></div>
    </div>
  </div>
{/if}

{#if clearingJob}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(event) => { if (!working && event.target === event.currentTarget) clearingJob = null; }}>
    <div class="modal" role="dialog" aria-modal="true" aria-label={t("Clear incomplete import")} tabindex="-1" {@attach focusOnMount}>
      <div class="modal-top"><div><h2>{t("Clear incomplete import")}</h2><p>{t("Source folders are never changed by this action.")}</p></div><button class="secondary" disabled={working} onclick={() => clearingJob = null}>{t("Close")}</button></div>
      {#each clearingJob.roots as root}<code class="import-root">{root}</code>{/each}
      {#if clearError}<div class="banner error" role="alert">{clearError}</div>{/if}
      {#if clearLoading}<p class="checking-files" role="status"><span class="activity-indicator" aria-hidden="true"></span>{t("Checking imported copies...")}</p>
      {:else if clearPreview}
        <label class="check-setting"><input type="radio" name="clear-import-mode" bind:group={trashImportCopies} value={false} disabled={working} /><span><strong>{t("Keep already copied files in the archive")}</strong><small>{t("Only dismiss this import from history. Nothing is removed from the archive or source folders.")}</small></span></label>
        <label class="check-setting"><input type="radio" name="clear-import-mode" bind:group={trashImportCopies} value={true} disabled={working || !clearPreview.tracksCopies || !clearPreview.removable} /><span><strong>{t("Move newly copied archive files to system Trash")}</strong><small>{clearPreview.removable} {t("archive copy/copies can be removed. Sources are re-verified and left untouched.")}</small></span></label>
        {#if !clearPreview.tracksCopies}<p class="banner warning">{t("This older import does not record which archive copies it created. For safety, its archive files must stay in place.")}</p>{/if}
        {#if clearPreview.retained}<p>{clearPreview.retained} {t("protected archive copy/copies will stay in place.")}</p>{/if}
        {#each clearPreview.reasons as reason}<p class="muted">{t(reason)}</p>{/each}
        <p class="muted">{t("Existing archive files and exact copies shared with other imports are always kept. Related files are protected together.")}</p>
        {#if trashImportCopies}<div class="banner warning">{t("This removes these copies from the archive. They can only be recovered through system Trash, not archive Undo. Do not empty Trash until you are sure.")}</div>{/if}
        {#if working}<p class="checking-files" role="status"><span class="activity-indicator" aria-hidden="true"></span>{t("Clearing import. Sources stay untouched...")}</p>{/if}
        <div class="modal-bottom button-row"><button class={trashImportCopies ? "danger-button" : "primary"} disabled={busy} onclick={clearImport}>{t(trashImportCopies ? "Move archive copies to Trash & clear import" : "Keep archive copies & clear import")}</button></div>
      {/if}
    </div>
  </div>
{/if}

{#if moves !== null}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(event) => { if (!working && event.target === event.currentTarget) moves = null; }}><div class="modal" role="dialog" aria-modal="true" aria-label={t("Archive reorganization preview")} tabindex="-1" {@attach focusOnMount}><div class="modal-top"><div><small>{t("PREVIEW BEFORE MOVING")}</small><h2>{t("Reorganize your archive.")}</h2></div><button class="secondary" disabled={working} onclick={() => moves = null}>{t("Close")}</button></div>{#if error}<div class="banner error" role="alert">{error}</div>{/if}<p class="muted">{t("Only pictures in your library move. Source files, spare copies of removed duplicates and original folder names stay unchanged.")}</p><div class="move-list">{#each moves as move}<div><code>{move.from}</code><small>{t("becomes")}</small><code>{move.to}</code></div>{/each}{#if !moves.length}<p>{t("Everything already follows the saved layout.")}</p>{/if}</div><div class="modal-bottom button-row"><span>{moves.length} {t("file(s) to reorganize")}</span><button class="primary" disabled={working || !moves.length} onclick={applyMoves}>{t("Confirm reorganization...")}</button></div></div></div>
{/if}

{#if undoAction}
  <div class="toast" role="status">
    <span>{t("Kept")} <strong>{undoAction.keptName}</strong>. {t("The duplicate was removed; a spare copy is kept.")}</span>
    <button class="text-button" title={busyReason} disabled={busy} onclick={undoKeep}>{t("Undo")}</button>
    <button class="text-button" aria-label={t("Dismiss")} onclick={() => { clearTimeout(undoTimer); undoAction = null; }}>×</button>
  </div>
{/if}

<style>
  :global(*) { box-sizing: border-box; }
  :global(body) { margin: 0; background: #f5f3ed; color: #21372d; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; font-size: calc(15px * var(--text-scale, 1)); -webkit-font-smoothing: antialiased; }
  :global(button), :global(input), :global(select) { font: inherit; }
  :global(button) { cursor: pointer; }
  :global(button:disabled) { cursor: not-allowed; opacity: .48; }
  :global(button:focus-visible), :global(input:focus-visible), :global(select:focus-visible), :global(summary:focus-visible) { outline: 3px solid #c9a75b; outline-offset: 3px; }
  :global(input), :global(select) { border: 1px solid #d8ddd2; background: #fffefb; color: #243b2d; border-radius: 8px; padding: 11px 13px; min-width: 0; }
  :global(code) { font-size: calc(14px * var(--text-scale, 1)); overflow-wrap: anywhere; }
  :global(h1), :global(h2), :global(h3), :global(p) { margin-top: 0; }
  :global(h1) { font-family: Georgia, "Times New Roman", serif; font-size: calc(39px * var(--text-scale, 1)); line-height: 1.15; font-weight: 500; letter-spacing: -.9px; margin-bottom: 13px; }
  :global(h2) { font-size: calc(22px * var(--text-scale, 1)); letter-spacing: -.4px; }
  :global(h3) { font-size: calc(16px * var(--text-scale, 1)); margin-bottom: 8px; }
  :global(small) { font-size: calc(13px * var(--text-scale, 1)); }
  :global(details) { margin: 15px 0; }
  :global(summary) { cursor: pointer; font-size: calc(14px * var(--text-scale, 1)); font-weight: 600; }
  .app-shell { display: flex; min-height: 100vh; }
  .viewer-shell { min-height: 100vh; padding: 0 28px 24px; }
  .viewer-header { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 20px 0; margin-bottom: 20px; border-bottom: 1px solid #dfe3d8; }
  .viewer-header strong { display: block; font-size: calc(16px * var(--text-scale, 1)); }
  .viewer-header small { display: block; color: var(--hc-muted, #85917c); margin-top: 6px; }
  .sidebar { width: calc(235px * var(--text-scale, 1)); flex-shrink: 0; background: #fdfcf8; border-right: 1px solid #e2e5dc; display: flex; flex-direction: column; padding: 30px 19px 24px; position: sticky; top: 0; height: 100vh; }
  .brand-symbol { display: grid; place-items: center; width: 41px; height: 41px; background: #274b38; color: #f2e9c9; border-radius: 11px; flex-shrink: 0; }
  .brand strong { display: block; font-size: calc(15px * var(--text-scale, 1)); letter-spacing: -.25px; }
  .brand small { display: block; margin-top: 4px; font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #8b9285); }
  .eyebrow { font-size: calc(11px * var(--text-scale, 1)); font-weight: 700; letter-spacing: .14em; }
  nav { display: flex; flex-direction: column; gap: 4px; }
  .sidebar-bottom { margin-top: auto; padding-top: 20px; }
  .display-controls { display: flex; flex-direction: column; gap: 8px; margin-bottom: 12px; }
  .text-size { display: flex; border: 1px solid #dfe3d8; border-radius: 8px; background: #fffdf8; overflow: hidden; }
  .text-size button { flex: 1; min-width: 0; border: 0; background: transparent; padding: 7px 0; color: var(--hc-muted, #526747); font-size: calc(13px * var(--text-scale, 1)); font-weight: 600; }
  .text-size button + button { border-left: 1px solid #dfe3d8; }
  .text-size button:hover:not(:disabled), .contrast-toggle:hover { background: #eef2e6; }
  .contrast-toggle { display: flex; align-items: center; justify-content: center; gap: 8px; width: 100%; border: 1px solid #dfe3d8; border-radius: 8px; background: #fffdf8; padding: 7px 8px; color: var(--hc-muted, #526747); font-size: calc(13px * var(--text-scale, 1)); font-weight: 600; }
  .contrast-toggle[aria-pressed="true"] { background: #274b38; border-color: #274b38; color: #fffef6; }
  :global(html.high-contrast) { --hc-muted: #26352b; }
  :global(html.high-contrast body) { color: #0f1d15; }
  :global(html.high-contrast button:disabled) { opacity: .62; }
  :global(html.high-contrast input:not(:focus)), :global(html.high-contrast select:not(:focus)) { border-color: #6b7a64; }
  :global(html.high-contrast input::placeholder) { color: #4d5b50; }
  :global(html.high-contrast) .lightbox-title small { color: #e3e9da; }
  :global(html.high-contrast) .text-size, :global(html.high-contrast) .text-size button + button, :global(html.high-contrast) .contrast-toggle:not([aria-pressed="true"]) { border-color: #6b7a64; }
  .language-setting { display: flex; flex-direction: column; gap: 6px; margin-top: 13px; color: var(--hc-muted, #788273); font-size: calc(12px * var(--text-scale, 1)); }
  .language-setting select { width: 100%; padding: 8px 10px; font-size: calc(13px * var(--text-scale, 1)); }
  .safe-dot { display: inline-block; height: 6px; width: 6px; background: #5c8753; border-radius: 50%; }
  .version { display: block; color: var(--hc-muted, #a6ac9d); font-size: calc(10px * var(--text-scale, 1)); text-align: center; letter-spacing: .13em; margin-top: 22px; }
  .content { flex: 1; min-width: 0; padding: 0 38px 20px; max-width: 1560px; }
  .topbar { height: 76px; display: flex; align-items: center; justify-content: space-between; border-bottom: 1px solid #e0e3d9; font-size: calc(13px * var(--text-scale, 1)); color: var(--hc-muted, #87917f); }
  .status { display: flex; align-items: center; gap: 7px; font-size: calc(12px * var(--text-scale, 1)); }
  .topbar-right { display: flex; align-items: center; gap: 14px; min-width: 0; }
  .archive-chip { display: inline-flex; align-items: center; gap: 7px; min-width: 0; max-width: calc(320px * var(--text-scale, 1)); border: 1px solid #dfe3d8; border-radius: 999px; background: #fffdf8; padding: 5px 12px; color: #2f4a37; font-size: calc(12px * var(--text-scale, 1)); font-weight: 600; }
  .archive-chip span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .archive-chip svg { flex-shrink: 0; color: #83936d; }
  .archive-chip:hover:not(:disabled) { background: #eef2e6; }
  .archive-chip.missing { border-color: #d6bd7e; background: #fffaf0; color: #7a5a17; }
  .archive-chip.missing svg { color: currentColor; }
  .page-heading { display: flex; justify-content: space-between; align-items: center; padding: 34px 0 24px; gap: 18px; }
  .eyebrow { color: var(--hc-muted, #8e9b82); margin-bottom: 15px; }
  .subtitle { color: var(--hc-muted, #7a8573); line-height: 1.7; font-size: calc(14px * var(--text-scale, 1)); margin-bottom: 0; }
  .page-heading.plain { padding: 30px 0 18px; }
  .page-heading.plain h1 { margin-bottom: 0; }
  .primary, .secondary, .danger-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 16px; border-radius: 8px; padding: 11px 17px; font-size: calc(13px * var(--text-scale, 1)); font-weight: 600; white-space: nowrap; }
  .primary { border: 1px solid #2a4e39; background: #2a4e39; color: #fffef6; box-shadow: 0 3px 6px #1d3c1b0f; }
  .primary:hover { background: #365f44; }
  .secondary { background: #fffdf8; border: 1px solid #d6dccf; color: #526747; }
  .secondary:hover { background: #f0f3e8; }
  .text-button { border: 0; background: transparent; color: var(--hc-muted, #6d805e); padding: 10px; }
  .danger-button { border: 1px solid #e5ccc3; background: #fff9f6; color: #9d493b; }
  .full { width: 100%; }
  .destination { display: flex; align-items: center; gap: 12px; border: 1px solid #dfe3d8; background: #fffdf8; padding: 11px 14px; border-radius: 10px; margin-bottom: 18px; }
  .disk-icon { background: #f1f2e8; color: var(--hc-muted, #83936d); padding: 8px; border-radius: 8px; display: grid; }
  .destination div { flex: 1; min-width: 0; }
  .destination small { color: var(--hc-muted, #939c87); font-size: calc(10px * var(--text-scale, 1)); font-weight: 700; letter-spacing: .12em; }
  .destination strong { display: block; font-size: calc(14px * var(--text-scale, 1)); margin-top: 4px; overflow-wrap: anywhere; }
  .destination p { margin: 6px 0 0; max-width: 60ch; color: var(--hc-muted, #75816c); font-size: calc(13px * var(--text-scale, 1)); line-height: 1.5; }
  .dropping { background: #e5ecd8 !important; border-color: #6d9056 !important; }
  .stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 15px; margin: 19px 0; }
  .stat { padding: 19px 21px; background: #fffdf8; border: 1px solid #e0e4d9; border-radius: 10px; }
  .stat small { color: var(--hc-muted, #939e86); font-size: calc(10px * var(--text-scale, 1)); font-weight: 700; letter-spacing: .1em; }
  .stat strong { display: block; font-size: calc(30px * var(--text-scale, 1)); font-weight: 500; letter-spacing: -1px; margin: 9px 0 5px; }
  .stat span { font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #919b87); }
  .next-steps { display: flex; justify-content: space-between; align-items: center; gap: 18px; padding: 21px 23px; border-radius: 11px; border: 1px solid #e0e3d8; background: #f9f8f1; margin: 20px 0; }
  .next-steps p { color: var(--hc-muted, #8a957f); font-size: calc(13px * var(--text-scale, 1)); line-height: 1.7; margin: 0; }
  .next-steps h3 { font-size: calc(15px * var(--text-scale, 1)); }
  .banner { padding: 13px 16px; border-radius: 8px; font-size: calc(14px * var(--text-scale, 1)); margin: 16px 0; line-height: 1.6; overflow-wrap: anywhere; }
  .banner.error { color: #984536; background: #faeae3; }
  div.banner.error { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .banner.warning { color: #8b6b2a; background: #f5ebcf; }
  .banner.success { color: #406239; background: #e8efdc; }
  .archive-activity { display: flex; align-items: center; justify-content: space-between; gap: 20px; border: 1px solid #d6bd7e; background: #f5ebcf; color: #655023; }
  .activity-copy { min-width: 0; }
  .activity-copy > strong { display: flex; align-items: center; gap: 9px; font-size: calc(15px * var(--text-scale, 1)); }
  .activity-copy p { margin: 6px 0 0; }
  .activity-indicator { width: 14px; height: 14px; flex-shrink: 0; border: 2px solid #d6bd7e; border-top-color: #80642b; border-radius: 50%; animation: activity-spin 1s linear infinite; }
  .activity-indicator.paused { animation: none; border: 0; border-left: 4px solid #80642b; border-right: 4px solid #80642b; border-radius: 0; width: 10px; height: 12px; }
  .activity-actions { display: flex; flex-direction: column; align-items: flex-end; gap: 9px; flex-shrink: 0; font-size: calc(13px * var(--text-scale, 1)); }
  .lightbox > .archive-activity { margin: 0 16px 12px; }
  @keyframes activity-spin { to { transform: rotate(360deg); } }
  @keyframes progress-slide { from { transform: translateX(-100%); } to { transform: translateX(290%); } }
  @media (max-width: 800px) {
    .archive-activity { flex-direction: column; align-items: stretch; gap: 12px; }
    .activity-actions { flex-direction: row; align-items: center; justify-content: space-between; flex-wrap: wrap; }
  }
  details.banner p { font-size: calc(13px * var(--text-scale, 1)); }
  .progress-card { border: 1px solid #dce3d4; background: #fffdf8; padding: 19px; border-radius: 10px; margin-top: 18px; }
  .section-line { display: flex; justify-content: space-between; align-items: center; gap: 12px; font-size: calc(13px * var(--text-scale, 1)); }
  .section-line strong { text-transform: capitalize; }
  .section-line span { color: var(--hc-muted, #929c85); }
  .progress-track { height: 5px; background: #e9eddf; margin: 13px 0 10px; border-radius: 5px; overflow: hidden; }
  .progress-track div { height: 100%; background: #6e8d54; transition: width .2s; }
  .progress-track.indeterminate:not(.paused) div { animation: progress-slide 1.5s ease-in-out infinite; }
  .progress-track.paused div { background: #a68a4b; }
  @media (prefers-reduced-motion: reduce) {
    .activity-indicator, .progress-track.indeterminate:not(.paused) div { animation: none; }
  }
  .current-file { font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #8a9680); min-block-size: 1lh; overflow: hidden; white-space: nowrap; text-overflow: ellipsis; margin-bottom: 0; }
  .import-stage { font-size: calc(14px * var(--text-scale, 1)); line-height: 1.6; }
  .import-stage p { margin: 5px 0; color: var(--hc-muted, #788273); }
  .stage-detail { min-block-size: 2lh; }
  .stage-detail span { display: block; }
  .import-queue code { overflow-wrap: anywhere; }
  .button-row { display: flex; align-items: center; gap: 10px; margin-top: 17px; flex-wrap: wrap; }
  .history { margin-top: 26px; }
  .history.incomplete-imports { margin-top: 0; }
  .history-row { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #e1e5d9; padding: 13px 0; }
  .history-row strong { display: block; font-size: calc(14px * var(--text-scale, 1)); margin-bottom: 5px; }
  .history-row small { color: var(--hc-muted, #8c987f); }
  .incomplete-notice { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .incomplete-notice p { margin: 4px 0 0; }
  .incomplete-import { border: 1px solid #d6bd7e; border-radius: 10px; padding: 18px; margin: 14px 0; background: #fffaf0; }
  .incomplete-import > p { line-height: 1.6; margin: 12px 0; }
  .import-folder { padding: 12px 0; border-top: 1px solid #e6decb; }
  .import-folder strong, .import-folder code, .import-folder small, .import-root { display: block; margin-bottom: 6px; }
  .import-folder p { margin: 6px 0 0; font-size: calc(14px * var(--text-scale, 1)); }
  .import-issues { overflow-wrap: anywhere; }
  .import-issues p { font-size: calc(14px * var(--text-scale, 1)); line-height: 1.6; }
  .checking-files { display: flex; align-items: flex-start; gap: 12px; padding: 22px 0; line-height: 1.6; }
  .checking-files .activity-indicator { margin-top: 4px; }
  .checking-files p { margin: 6px 0 0; color: var(--hc-muted, #788273); }
  .cleanup-folder { border-bottom: 1px solid #dfe4d6; }
  .cleanup-file-options { margin: 0 0 12px 28px; }
  .cleanup-file-options summary { line-height: 1.6; }
  .empty { padding: 80px 20px; text-align: center; color: var(--hc-muted, #748468); background: #f0f2e9; border-radius: 13px; border: 1px dashed #d8dfcd; }
  .empty h2 { font-family: Georgia, serif; font-weight: 400; color: #4a6641; margin-bottom: 12px; }
  .empty p { max-width: 62ch; margin-left: auto; margin-right: auto; line-height: 1.55; font-size: calc(14px * var(--text-scale, 1)); }
  .empty-check { display: inline-grid; place-items: center; background: #e0ebd4; border-radius: 50%; width: 50px; height: 50px; margin-bottom: 19px; font-size: calc(13px * var(--text-scale, 1)); }
  .pill { border-radius: 30px; background: #e6eddc; padding: 9px 14px; color: var(--hc-muted, #6b8654); font-size: calc(13px * var(--text-scale, 1)); white-space: nowrap; }
  .review-actions { display: flex; align-items: center; gap: 12px; }
  .review-list { display: flex; flex-direction: column; gap: 14px; }
  .disabled-hint:has(> button:disabled) { cursor: not-allowed; }
  .disabled-hint > button:disabled { pointer-events: none; }
  .review-row { display: flex; align-items: center; gap: 19px; padding: 23px; width: 100%; text-align: left; color: inherit; background: #fffdf8; border: 1px solid #dfe4d6; border-radius: 12px; cursor: pointer; }
  .review-row:hover:not(:disabled) { border-color: #aab99a; background: #fbfcf6; }
  .review-row:focus-visible { outline: 3px solid #879c70; outline-offset: 3px; }
  .review-description { flex: 1; min-width: 0; }
  .review-row strong { font-size: calc(14px * var(--text-scale, 1)); }
  .review-description > span { display: block; font-size: calc(13px * var(--text-scale, 1)); line-height: 1.6; color: var(--hc-muted, #8b977f); margin: 7px 0 0; }
  .review-thumbnails { display: flex; gap: 6px; flex-shrink: 0; }
  .review-thumbnails :global(.preview) { width: 72px; height: 72px; }
  .review-thumbnails :global(.placeholder) { padding: 4px; gap: 2px; }
  .review-thumbnails :global(.placeholder svg) { width: 20px; height: 20px; }
  .review-thumbnails :global(.placeholder span), .review-thumbnails :global(.placeholder small) { font-size: calc(10px * var(--text-scale, 1)); max-height: 22px; overflow: hidden; }
  .review-thumbnails :global(.type) { right: 3px; bottom: 3px; padding: 2px 3px; font-size: calc(10px * var(--text-scale, 1)); }
  .settings-card { background: #fffdf8; border: 1px solid #dfe3d6; border-radius: 12px; padding: 27px; margin-bottom: 20px; }
  .settings-card h3 { margin-bottom: 21px; }
  .settings-card label { display: flex; flex-direction: column; gap: 8px; font-size: calc(13px * var(--text-scale, 1)); color: var(--hc-muted, #788a69); }
  .setting-columns { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; margin-top: 21px; }
  .advanced { padding: 13px 0; border-bottom: 1px solid #e3e7da; }
  .advanced label { margin-top: 17px; }
  .advanced p { font-size: calc(13px * var(--text-scale, 1)); color: var(--hc-muted, #8c997f); margin: 11px 0 0; line-height: 1.7; }
  .path-preview { background: #eff2e7; padding: 19px; border-radius: 8px; margin: 22px 0; display: flex; flex-direction: column; gap: 9px; }
  .path-preview small { font-size: calc(10px * var(--text-scale, 1)); letter-spacing: .11em; font-weight: 600; color: var(--hc-muted, #8e9c7f); margin-bottom: 5px; }
  .path-preview code { color: var(--hc-muted, #5f7850); font-size: calc(13px * var(--text-scale, 1)); }
  .path-preview p { font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #8b9a7c); margin: 9px 0 0; }
  .check-setting { display: flex !important; flex-direction: row !important; align-items: flex-start; gap: 10px !important; margin: 18px 0; }
  .check-setting input { accent-color: #496f3c; margin-top: 2px; }
  .check-setting strong, .check-setting small { display: block; }
  .check-setting strong { font-size: calc(14px * var(--text-scale, 1)); color: #4e6941; }
  .check-setting small { font-size: calc(12px * var(--text-scale, 1)); line-height: 1.6; color: var(--hc-muted, #93a082); margin-top: 5px; }
  .muted { color: var(--hc-muted, #8a997c); font-size: calc(13px * var(--text-scale, 1)); line-height: 1.7; }
  .field-error { color: #ac5944 !important; }
  .tool-list { display: flex; flex-direction: column; gap: 12px; margin-top: 20px; }
  .tool-list div { display: flex; align-items: center; gap: 10px; }
  .tool-list strong { font-size: calc(13px * var(--text-scale, 1)); min-width: 95px; }
  .tool-list small { color: var(--hc-muted, #9ba48f); overflow-wrap: anywhere; }
  .unavailable { background: #c88152; }
  footer { font-size: calc(11px * var(--text-scale, 1)); color: var(--hc-muted, #a7af9a); padding-top: 27px; display: flex; justify-content: space-between; gap: 20px; }
  .modal-backdrop { position: fixed; inset: 0; background: #13261acf; z-index: 10; overflow: auto; display: flex; align-items: flex-start; justify-content: center; padding: 30px 22px; }
  .modal { position: relative; margin: 0; border: 0; color: inherit; width: min(760px, 100%); background: #fffdf8; border-radius: 15px; padding: 27px; box-shadow: 0 20px 100px #0003; max-height: calc(100vh - 60px); overflow: auto; }
  .comparison-dismiss { width: 100%; margin-top: 8px; }
  .modal-top { display: flex; align-items: center; justify-content: space-between; gap: 20px; margin-bottom: 22px; }
  .modal-top small { font-size: calc(10px * var(--text-scale, 1)); letter-spacing: .12em; color: var(--hc-muted, #92a17f); }
  .modal-top h2 { font-family: Georgia, serif; font-weight: 400; margin: 8px 0 0; }
  .detail-columns { display: grid; grid-template-columns: 1fr; gap: 24px; }
  .comparison .detail-columns { grid-template-columns: 1fr 1fr; }
  .detail-column { min-width: 0; }
  .source-history { margin-top: 12px; font-size: calc(13px * var(--text-scale, 1)); display: flex; flex-direction: column; gap: 5px; }
  .source-history code, .source-history small { font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #96a386); }
  .modal-bottom { border-top: 1px solid #e2e6d8; padding-top: 20px; margin-top: 21px; }
  .cleanup-list, .move-list { max-height: 350px; overflow: auto; }
  .cleanup-row { display: flex; align-items: flex-start; gap: 12px; border-bottom: 1px solid #e2e6d8; padding: 15px 0; }
  .cleanup-row input { accent-color: #466b3c; }
  .cleanup-row span { flex: 1; min-width: 0; }
  .cleanup-row strong { display: block; font-size: calc(13px * var(--text-scale, 1)); overflow-wrap: anywhere; font-weight: 500; }
  .cleanup-row small { color: var(--hc-muted, #95a486); font-size: calc(12px * var(--text-scale, 1)); display: block; margin-top: 5px; }
  .cleanup-result { font-size: calc(13px * var(--text-scale, 1)); color: var(--hc-muted, #7f916d); overflow-wrap: anywhere; }
  .move-list div { display: flex; flex-direction: column; gap: 7px; padding: 15px 0; border-bottom: 1px solid #e2e6d8; }
  .move-list small { color: var(--hc-muted, #9ba88e); }
  .mini { min-height: 100vh; background: #f8f8ef; padding: 15px; display: flex; flex-direction: column; }
  .mini-heading { display: flex; gap: 9px; align-items: center; margin-bottom: 12px; font-size: calc(14px * var(--text-scale, 1)); }
  .mini .brand-symbol { width: 28px; height: 28px; border-radius: 8px; font-size: calc(10px * var(--text-scale, 1)); }
  .mini-drop { display: flex; flex: 1; flex-direction: column; align-items: center; justify-content: center; width: 100%; padding: 24px 12px; border: 1px dashed #b7ca9f; border-radius: 10px; background: #eef3e3; color: var(--hc-muted, #567447); gap: 10px; }
  .mini-drop strong { font-size: calc(14px * var(--text-scale, 1)); }
  .mini-drop small { font-size: calc(12px * var(--text-scale, 1)); color: var(--hc-muted, #8b9d78); }
  .mini-bottom { display: flex; justify-content: space-between; align-items: center; margin-top: 10px; color: var(--hc-muted, #8c9e78); }
  .mini-bottom .text-button { font-size: calc(12px * var(--text-scale, 1)); }
  .mini-bottom small, .mini-pin { font-size: calc(11px * var(--text-scale, 1)); }
  .mini-pin { display: flex; align-items: center; gap: 6px; color: var(--hc-muted, #8b9c7a); }
  .mini-pin input { accent-color: #536f40; }
  .mini-error { font-size: calc(12px * var(--text-scale, 1)); color: #a65b45; overflow-wrap: anywhere; margin-top: 11px; }
  .mini-notice { color: var(--hc-muted, #788273); }
  .brand { display: flex; align-items: center; gap: 10px; margin: 0 4px 26px; }
  .add-folders { gap: 8px; padding: 12px 14px; font-size: calc(14px * var(--text-scale, 1)); }
  .drop-hint { color: var(--hc-muted, #7d8676); font-size: calc(13px * var(--text-scale, 1)); line-height: 1.5; text-align: center; margin: 9px 0 18px; }
  .sidebar-progress { display: flex; flex-direction: column; gap: 7px; width: 100%; border: 1px solid #dce3d4; background: #f6f8f0; border-radius: 9px; padding: 10px 12px; margin: -6px 0 18px; color: #3f5a36; text-align: left; }
  .sidebar-progress > span:first-child { display: flex; justify-content: space-between; font-size: calc(13px * var(--text-scale, 1)); }
  .sidebar-progress strong { text-transform: capitalize; }
  .sidebar-progress .progress-track { display: block; margin: 0; }
  nav button { border: 0; background: transparent; color: var(--hc-muted, #5f6c5a); text-align: left; padding: 11px 11px; display: flex; align-items: center; gap: 11px; font-size: calc(14px * var(--text-scale, 1)); border-radius: 8px; }
  nav button:hover { background: #f1f4ec; }
  nav button.active { color: #274b38; background: #eaf0e4; font-weight: 600; }
  .nav-icon { flex-shrink: 0; opacity: .8; width: calc(18px * var(--text-scale, 1)); height: calc(18px * var(--text-scale, 1)); }
  .nav-count { margin-left: auto; border-radius: 5px; background: #dde8d4; padding: 2px 6px; font-size: calc(13px * var(--text-scale, 1)); font-weight: 600; }
  .nav-count.attention { background: #f3e2b8; color: #7a5a17; }
  .library-page { padding-top: 22px; }
  .library-toolbar { display: flex; align-items: center; gap: 12px; margin: 0 0 22px; flex-wrap: wrap; }
  .search-field { flex: 1; min-width: 220px; position: relative; display: flex; align-items: center; color: var(--hc-muted, #7d8a72); }
  .search-field svg { position: absolute; left: 13px; pointer-events: none; }
  .search { flex: 1; padding-left: 38px !important; padding-right: 34px !important; }
  .clear-search { position: absolute; right: 6px; border: 0; background: transparent; color: var(--hc-muted, #6d7a64); font-size: calc(18px * var(--text-scale, 1)); line-height: 1; padding: 4px 8px; border-radius: 6px; }
  .clear-search:hover { background: #eef1e7; }
  .size-slider { display: flex; align-items: center; gap: 7px; color: var(--hc-muted, #8b977f); }
  .size-slider input { padding: 0; border: 0; background: transparent; width: 110px; accent-color: #496f3c; }
  .library-layout { display: flex; align-items: flex-start; gap: 26px; }
  .timeline { position: sticky; top: 16px; width: 128px; flex-shrink: 0; display: flex; flex-direction: column; gap: 2px; max-height: calc(100vh - 32px); overflow: auto; padding-bottom: 12px; }
  .timeline button { display: flex; justify-content: space-between; align-items: baseline; gap: 8px; border: 0; background: transparent; color: var(--hc-muted, #5f6c5a); padding: 7px 10px; border-radius: 7px; text-align: left; font-size: calc(14px * var(--text-scale, 1)); }
  .timeline button:hover { background: #eef1e7; }
  .timeline small { color: var(--hc-muted, #8b977f); font-size: calc(13px * var(--text-scale, 1)); }
  .timeline-year.active { color: #274b38; font-weight: 700; }
  .timeline-months { display: flex; flex-direction: column; gap: 1px; margin: 0 0 8px 8px; padding-left: 6px; border-left: 1px solid #dfe4d6; }
  .timeline-months button { font-size: calc(14px * var(--text-scale, 1)); padding: 5px 9px; text-transform: capitalize; }
  .timeline-months button.active { background: #e3ecda; color: #274b38; font-weight: 600; }
  .library-main { flex: 1; min-width: 0; }
  .month-header { display: flex; justify-content: space-between; align-items: center; gap: 16px; margin-bottom: 16px; }
  .month-header h2 { font-family: Georgia, serif; font-weight: 500; font-size: calc(26px * var(--text-scale, 1)); margin: 0 0 4px; text-transform: capitalize; }
  .month-header span { color: var(--hc-muted, #75816c); font-size: calc(14px * var(--text-scale, 1)); }
  .month-header .button-row { margin: 0; flex-wrap: nowrap; }
  .load-earlier { margin: 0 0 12px; }
  .photo-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(var(--thumb, 200px), 1fr)); gap: 8px; transition: opacity .15s; }
  .photo-grid.loading { opacity: .55; }
  .photo-grid :global(.preview) { height: auto; aspect-ratio: 1 / 1; border-radius: 8px; }
  .photo-card { position: relative; border: 0; padding: 0; border-radius: 8px; background: transparent; text-align: left; min-width: 0; color: white; overflow: hidden; }
  .photo-card:hover :global(img) { filter: brightness(1.05); }
  .photo-card:focus-visible { outline-offset: 2px; }
  .photo-caption { position: absolute; left: 0; right: 0; bottom: 0; padding: 22px 10px 8px; background: linear-gradient(transparent, #0d1a12c2); opacity: 0; transition: opacity .12s; pointer-events: none; border-radius: 0 0 8px 8px; }
  .photo-card:hover .photo-caption, .photo-card:focus-visible .photo-caption { opacity: 1; }
  .photo-caption strong, .photo-caption small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .photo-caption strong { font-size: calc(14px * var(--text-scale, 1)); font-weight: 600; }
  .photo-caption small { font-size: calc(13px * var(--text-scale, 1)); opacity: .85; margin-top: 3px; }
  .card-badge { position: absolute; top: 8px; left: 8px; background: #f5ebcf; color: #6f5418; border-radius: 5px; padding: 3px 7px; font-size: calc(12px * var(--text-scale, 1)); font-weight: 700; }
  .load-more { min-height: 60px; display: grid; place-items: center; color: var(--hc-muted, #75816c); font-size: calc(14px * var(--text-scale, 1)); }
  .drop-overlay { position: fixed; inset: 0; z-index: 30; display: grid; place-items: center; background: #1d3a2bb8; pointer-events: none; }
  .drop-overlay div { display: flex; flex-direction: column; align-items: center; gap: 10px; padding: 40px 56px; border: 2px dashed #c9dbb6; border-radius: 18px; background: #f5f8ee; color: #2f5233; }
  .drop-overlay strong { font-family: Georgia, serif; font-weight: 500; font-size: calc(24px * var(--text-scale, 1)); }
  .drop-overlay small { font-size: calc(14px * var(--text-scale, 1)); color: var(--hc-muted, #61745a); }
  .pill.unsaved { background: #f5ebcf; color: #7a5a17; padding: 7px 12px; }
  kbd { font: inherit; font-size: calc(12px * var(--text-scale, 1)); font-weight: 700; border: 1px solid currentColor; border-radius: 4px; padding: 1px 5px; opacity: .75; }
  .key-hint { color: var(--hc-muted, #7d8a72); font-size: calc(13px * var(--text-scale, 1)); margin: 14px 0 0; }
  .lightbox { position: fixed; inset: 0; z-index: 20; background: #0f1b15; color: #eef1e7; display: flex; flex-direction: column; outline: none; }
  .lightbox-top { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 10px 14px; border-bottom: 1px solid #ffffff14; }
  .lightbox-title { flex: 1; min-width: 0; text-align: center; }
  .lightbox-title strong, .lightbox-title small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .lightbox-title strong { font-size: calc(14px * var(--text-scale, 1)); }
  .lightbox-title small { color: #a9b6a0; font-size: calc(13px * var(--text-scale, 1)); margin-top: 3px; text-transform: capitalize; }
  .lightbox-tools { display: flex; gap: 6px; }
  .lightbox-button { border: 1px solid #ffffff24; background: transparent; color: #eef1e7; border-radius: 8px; padding: 8px 13px; font-size: calc(14px * var(--text-scale, 1)); font-weight: 600; }
  .lightbox-button:hover, .lightbox-button.active { background: #ffffff18; }
  .lightbox-error { margin: 10px 14px 0; }
  .lightbox-body { flex: 1; min-height: 0; display: flex; }
  .lightbox-stage { flex: 1; min-width: 0; position: relative; display: grid; place-items: center; padding: 14px 64px; }
  .lightbox-stage :global(.preview.large) { width: 100%; height: 100%; background: transparent; border-radius: 0; }
  .lightbox-stage :global(.placeholder) { color: #a9b6a0; }
  .lightbox-stage video { width: 100%; height: 100%; max-height: none; margin: 0; object-fit: contain; background: transparent; }
  .lightbox-nav { position: absolute; top: 50%; transform: translateY(-50%); z-index: 1; width: 46px; height: 64px; border: 0; border-radius: 10px; background: #ffffff12; color: white; font-size: calc(34px * var(--text-scale, 1)); line-height: 1; }
  .lightbox-nav:hover:not(:disabled) { background: #ffffff26; }
  .lightbox-nav:disabled { opacity: .2; }
  .lightbox-nav.previous { left: 10px; }
  .lightbox-nav.next { right: 10px; }
  .play-overlay { position: absolute; inset: 0; display: grid; place-items: center; pointer-events: none; }
  .play-overlay > * { pointer-events: auto; }
  .play-overlay span { background: #0f1b15d0; padding: 10px 16px; border-radius: 8px; font-size: calc(14px * var(--text-scale, 1)); }
  .lightbox-info { width: 320px; flex-shrink: 0; overflow: auto; padding: 18px 20px; background: #fffdf8; color: #21372d; border-left: 1px solid #ffffff14; }
  .lightbox-info dl { margin: 0 0 16px; display: grid; gap: 4px; }
  .lightbox-info dt { font-size: calc(12px * var(--text-scale, 1)); font-weight: 700; letter-spacing: .1em; text-transform: uppercase; color: var(--hc-muted, #7d8a72); margin-top: 10px; }
  .lightbox-info dd { margin: 0; font-size: calc(14px * var(--text-scale, 1)); overflow-wrap: anywhere; }
  .lightbox-info dd small { display: block; color: var(--hc-muted, #75816c); margin-top: 3px; }
  .lightbox-info dd code { font-size: calc(13px * var(--text-scale, 1)); color: var(--hc-muted, #5f7850); }
  .lightbox-actions { display: flex; flex-direction: column; gap: 10px; border-top: 1px solid #e2e6d8; padding-top: 16px; margin-top: 8px; }
  .lightbox-actions label { display: flex; flex-direction: column; gap: 6px; font-size: calc(14px * var(--text-scale, 1)); color: var(--hc-muted, #61745a); }
  .lightbox-info .key-hint { color: var(--hc-muted, #8b977f); }
  .comparison { width: min(1240px, 100%); }
  .comparison .modal-top { margin-bottom: 14px; }
  .button-row.compact { margin: 0; }
  .compare-pane { position: relative; border-radius: 10px; overflow: hidden; cursor: zoom-in; }
  .compare-pane :global(.preview.large) { height: clamp(260px, calc(100vh - 380px), 680px); }
  .compare-pane :global(img) { transition: transform .08s; }
  .compare-pane.zoomed { cursor: zoom-out; }
  .compare-pane.zoomed :global(img) { transform: scale(2.6); transform-origin: var(--zx) var(--zy); }
  .compare-pane video { margin: 0; }
  .keep-button { width: 100%; flex-direction: column; gap: 4px; margin-top: 10px; white-space: normal; padding: 12px 16px; font-size: calc(14px * var(--text-scale, 1)); }
  .keep-button small { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: calc(13px * var(--text-scale, 1)); font-weight: 400; opacity: .85; }
  .detail-column > .muted { margin: 6px 0 0; }
  .comparison-dismiss { width: 100%; margin-top: 10px; }
  .quality-label { position: absolute; top: 10px; left: 10px; padding: 5px 9px; border-radius: 6px; background: #e1efd8; color: #24521f; font-size: calc(14px * var(--text-scale, 1)); font-weight: 700; box-shadow: 0 2px 8px #0002; }
  .quality-higher { color: #2c6a26; font-weight: 700; }
  .quality-higher small { font-size: calc(13px * var(--text-scale, 1)); }
  .difference-table { width: 100%; border-collapse: collapse; margin-top: 18px; font-size: calc(14px * var(--text-scale, 1)); table-layout: fixed; }
  .difference-table th { width: 150px; text-align: left; font-weight: 600; color: var(--hc-muted, #75816c); padding: 8px 10px 8px 0; vertical-align: top; }
  .difference-table td { padding: 8px 10px; vertical-align: top; overflow-wrap: anywhere; border-top: 1px solid #edf0e6; }
  .difference-table th { border-top: 1px solid #edf0e6; }
  .difference-table tr.differs td { background: #f7f9f1; font-weight: 600; }
  .difference-table code { font-size: calc(13px * var(--text-scale, 1)); font-weight: 400; color: var(--hc-muted, #5f7850); }
  .difference-table td > small:not(.cell-note) { margin-left: 5px; }
  [role="dialog"][tabindex="-1"]:focus { outline: none; }
  .cell-note { display: block; font-weight: 400; color: var(--hc-muted, #75816c); margin-top: 3px; }
  video { width: 100%; max-height: 340px; margin-top: 13px; background: #182b1c; border-radius: 8px; }
  .toast { position: fixed; left: 50%; bottom: 24px; transform: translateX(-50%); z-index: 40; display: flex; align-items: center; gap: 8px; max-width: min(640px, calc(100vw - 32px)); padding: 10px 10px 10px 18px; border-radius: 10px; background: #21372d; color: #f3f1e6; box-shadow: 0 10px 40px #0004; font-size: calc(14px * var(--text-scale, 1)); }
  .toast span { overflow: hidden; text-overflow: ellipsis; }
  .toast .text-button { color: #d9e8c6; font-size: calc(14px * var(--text-scale, 1)); }
  .toast .text-button:hover { background: #ffffff14; }
  @media (max-width: 1000px) { .sidebar { width: calc(204px * var(--text-scale, 1)); padding: 25px 13px; } .content { padding: 0 25px 20px; } :global(h1) { font-size: calc(33px * var(--text-scale, 1)); } footer span { display: none; } }
  @media (max-width: 760px) { .topbar .status { display: none; } .sidebar { width: calc(170px * var(--text-scale, 1)); padding: 20px 9px; } .brand small, .drop-hint { display: none; } .timeline { width: 96px; } .lightbox-info { position: absolute; left: 0; right: 0; bottom: 0; width: auto; max-height: 42%; box-shadow: 0 -10px 30px #0005; z-index: 2; } .lightbox-body { position: relative; } .size-slider { display: none; } .brand { margin: 0 3px 30px; } .brand strong { font-size: calc(14px * var(--text-scale, 1)); } .content { padding: 0 18px 20px; } .stats { gap: 8px; } .stat { padding: 14px 12px; } .stat strong { font-size: calc(23px * var(--text-scale, 1)); } .next-steps { align-items: flex-start; flex-direction: column; } .comparison .detail-columns { grid-template-columns: 1fr; } .setting-columns { grid-template-columns: 1fr; } }
</style>
