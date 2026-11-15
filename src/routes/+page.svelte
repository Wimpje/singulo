<script lang="ts">
  import { onMount } from "svelte";
  import { isTauri, convertFileSrc } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { open, confirm } from "@tauri-apps/plugin-dialog";
  import Preview from "$lib/Preview.svelte";
  import {
    api, bytes, defaults, emptySnapshot, message,
    type Snapshot, type Settings, type Progress, type Item, type Details,
    type Review, type CleanupRow, type MoveRow, type Job, type ToolStatus,
  } from "$lib/api";

  type Tab = "home" | "library" | "review" | "settings";
  type LibraryMonth = { month: string; count: number };
  const pageSize = 60;
  let native = $state(false);
  let mode = $state("main");
  let tab = $state<Tab>("home");
  let snapshot = $state<Snapshot>(structuredClone(emptySnapshot));
  let config = $state<Settings>({ ...defaults });
  let error = $state("");
  let notice = $state("");
  let working = $state(false);
  let dropping = $state(false);
  let items = $state<Item[]>([]);
  let reviews = $state<Review[]>([]);
  let jobs = $state<Job[]>([]);
  let tools = $state<ToolStatus[]>([]);
  let search = $state("");
  let filter = $state("all");
  let months = $state<LibraryMonth[]>([]);
  let month = $state("");
  let page = $state(0);
  let libraryLoading = $state(false);
  let examples = $state<string[]>([]);
  let configError = $state("");
  let selected = $state<Details | null>(null);
  let comparing = $state<Details | null>(null);
  let currentReview = $state<Review | null>(null);
  let skipKeeperConfirmation = $state(false);
  let dateEntry = $state("");
  let videoSource = $state("");
  let secondVideoSource = $state("");
  let firstVideo = $state<HTMLVideoElement>();
  let secondVideo = $state<HTMLVideoElement>();
  let cleanup = $state<CleanupRow[] | null>(null);
  let cleanupSelection = $state<string[]>([]);
  let emptyFolders = $state(false);
  let cleanupResults = $state<string[]>([]);
  let moves = $state<MoveRow[] | null>(null);
  let undoingMoves = $state(false);
  let onTop = $state(true);
  let loaded = $state(false);
  let lastQuery = "";
  let searchSeq = 0;
  let mutating = false;
  const busy = $derived(working || ["scanning", "importing", "paused"].includes(snapshot.progress.status));
  const percent = $derived(snapshot.progress.total ? Math.round(snapshot.progress.done / snapshot.progress.total * 100) : 0);
  const viewerIndex = $derived(selected ? items.findIndex((item) => item.id === selected?.item.id) : -1);
  const monthCount = $derived(months.find((entry) => entry.month === month)?.count ?? 0);
  const years = $derived([...new Set(months.map((entry) => entry.month === "unknown" ? "unknown" : entry.month.slice(0, 4)))]);
  const year = $derived(month === "unknown" ? "unknown" : month.slice(0, 4));
  const presets = [
    ["{yyyy}/{mm}/{dd}", "Year / month / day"],
    ["{yyyy}/{mm}", "Year / month"],
    ["{yyyy}-{mm}-{dd}", "Date folders"],
    ["{media_type}/{yyyy}/{mm}/{dd}", "Photos & videos / year / month / day"],
  ];

  async function run(action: () => Promise<void>) {
    working = true;
    error = "";
    notice = "";
    try { await action(); }
    catch (reason: unknown) { error = message(reason); console.error(reason); }
    finally { working = false; }
  }

  async function refresh(preservePages = false) {
    snapshot = await api<Snapshot>("snapshot");
    if (snapshot.root) {
      if (mode === "main") {
        [reviews, jobs] = await Promise.all([api<Review[]>("review_list"), api<Job[]>("job_history")]);
      }
      await loadLibrary(preservePages);
    } else {
      items = [];
      months = [];
      month = "";
      page = 0;
      lastQuery = "";
      libraryLoading = false;
      searchSeq += 1;
    }
  }

  async function mutate(action: () => Promise<void>) {
    mutating = true;
    try { await action(); }
    finally { mutating = false; }
  }

  async function loadLibrary(preservePage = false, seq = ++searchSeq) {
    const key = `${search}\0${filter}`;
    const currentSearch = search;
    const currentFilter = filter;
    const result = await api<LibraryMonth[]>("library_months", { search: currentSearch, filter: currentFilter });
    if (seq !== searchSeq) return;
    const nextMonth = result.some((entry) => entry.month === month) ? month : result[0]?.month ?? "";
    const count = result.find((entry) => entry.month === nextMonth)?.count ?? 0;
    const nextPage = preservePage && key === lastQuery && nextMonth === month
      ? Math.min(page, Math.max(0, Math.ceil(count / pageSize) - 1)) : 0;
    const nextItems = nextMonth
      ? await api<Item[]>("library", { search: currentSearch, filter: currentFilter, month: nextMonth, offset: nextPage * pageSize })
      : [];
    if (seq !== searchSeq) return;
    months = result;
    month = nextMonth;
    page = nextPage;
    items = nextItems;
    lastQuery = key;
    libraryLoading = false;
  }

  async function changeMonth(nextMonth: string) {
    if (nextMonth === month || working) return;
    await run(async () => {
      const seq = ++searchSeq;
      const result = await api<Item[]>("library", { search, filter, month: nextMonth, offset: 0 });
      if (seq !== searchSeq) return;
      month = nextMonth;
      page = 0;
      items = result;
    });
  }

  async function changePage(nextPage: number) {
    if (working || nextPage < 0 || nextPage * pageSize >= monthCount) return;
    await run(async () => {
      const seq = ++searchSeq;
      const result = await api<Item[]>("library", { search, filter, month, offset: nextPage * pageSize });
      if (seq !== searchSeq) return;
      page = nextPage;
      items = result;
    });
  }

  async function chooseArchive() {
    await run(async () => {
      const path = await open({ directory: true, multiple: false, title: "Choose the archive destination (a dedicated folder is best)" });
      if (!path) return;
      await mutate(async () => {
        snapshot = await api<Snapshot>("open_archive", { path });
        config = { ...snapshot.settings };
        await refresh();
      });
      loaded = true;
    });
  }

  async function importFolders(paths?: string[]) {
    await run(async () => {
      if (!snapshot.root) throw new Error("Choose an archive destination first.");
      const folders = paths ?? await open({ directory: true, multiple: true, title: "Choose photo folders to copy" });
      if (!folders) return;
      const started = await api<boolean>("start_import", { roots: typeof folders === "string" ? [folders] : folders });
      if (started) snapshot.progress = { ...snapshot.progress, status: "scanning", done: 0, total: 0, errors: [] };
      notice = started ? "Folders added. Originals stay untouched during import." : "Folders queued after the current import.";
    });
  }

  async function selectItem(id: string) {
    await run(async () => {
      selected = await api<Details>("item_details", { id });
      comparing = null;
      currentReview = null;
      dateEntry = selected.item.media.date?.slice(0, 10) ?? "";
      videoSource = "";
      secondVideoSource = "";
    });
  }

  async function navigatePhoto(direction: -1 | 1) {
    if (working || viewerIndex < 0) return;
    const absoluteIndex = page * pageSize + viewerIndex + direction;
    if (absoluteIndex < 0 || absoluteIndex >= monthCount) return;
    await run(async () => {
      const nextPage = Math.floor(absoluteIndex / pageSize);
      const seq = ++searchSeq;
      const nextItems = nextPage === page ? items
        : await api<Item[]>("library", { search, filter, month, offset: nextPage * pageSize });
      const item = nextItems[absoluteIndex % pageSize];
      if (!item) throw new Error("The photo list changed. Refresh the library and try again.");
      const detail = await api<Details>("item_details", { id: item.id });
      if (seq !== searchSeq) return;
      items = nextItems;
      page = nextPage;
      selected = detail;
      dateEntry = detail.item.media.date?.slice(0, 10) ?? "";
      videoSource = "";
      secondVideoSource = "";
    });
  }

  function viewerKey(event: KeyboardEvent) {
    if (working) return;
    const target = event.target;
    if (target instanceof HTMLElement && (target.isContentEditable || target.closest("input, select, textarea, video"))) return;
    if (event.key === "Escape" && selected) {
      closeDetails();
      return;
    }
    if (selected && !comparing && !currentReview && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
      event.preventDefault();
      void navigatePhoto(event.key === "ArrowLeft" ? -1 : 1);
    }
  }

  async function loadComparison(review: Review) {
    [selected, comparing] = await Promise.all([
      api<Details>("item_details", { id: review.item }),
      api<Details>("item_details", { id: review.candidate }),
    ]);
    currentReview = review;
    videoSource = "";
    secondVideoSource = "";
  }

  async function compare(review: Review) {
    await run(async () => { await loadComparison(review); });
  }

  async function resolve(keeper: string | null) {
    const review = currentReview;
    if (!review) return;
    await run(async () => {
      if (keeper && !skipKeeperConfirmation && !await confirm("Keep the chosen file active and move the other original into archive undo? Meaningful edits and RAW/JPEG pairs should be kept together.", { title: "Confirm your keeper", kind: "warning" })) return;
      await mutate(async () => {
        await api("resolve_review", { ...review, keeper });
        selected = null; comparing = null; currentReview = null;
        await refresh(true);
        if (reviews.length) await loadComparison(reviews[0]);
        else notice = "All similar matches have been reviewed.";
      });
    });
  }

  function updateSkipKeeperConfirmation() {
    window.localStorage.setItem("photo-dedupe.skipKeeperConfirmation", String(skipKeeperConfirmation));
  }

  async function playVideo(item: Item, second = false) {
    await run(async () => {
      const path = await api<string>("video_preview", { id: item.id });
      if (second) secondVideoSource = convertFileSrc(path);
      else videoSource = convertFileSrc(path);
    });
  }

  async function startCleanup() {
    await run(async () => {
      cleanup = await api<CleanupRow[]>("cleanup_preview");
      cleanupSelection = cleanup.filter((row) => row.eligible).map((row) => row.id);
      cleanupResults = [];
    });
  }

  async function cleanSources() {
    await run(async () => {
      if (!cleanupSelection.length) throw new Error("Select at least one eligible file.");
      const approved = await confirm(
        `Move ${cleanupSelection.length} verified source files to OS Trash/Recycle Bin?\n\nThis may leave your archive as the only copy. A second backup is recommended.\n\nTrash must usually be emptied manually before disk space is reclaimed. The app never permanently deletes sources.`,
        { title: "Clean up verified sources", kind: "warning" },
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

  async function applyMoves() {
    await run(async () => {
      if (!moves?.length || !await confirm(`Reorganize ${moves.length} archived files using the saved layout? Sources are untouched. A recovery manifest is saved with the archive.`, { title: "Reorganize archive", kind: "warning" })) return;
      await mutate(async () => {
        await api(undoingMoves ? "rollback_archive" : "reorganize_archive", { preview: moves, confirmed: true });
        moves = null;
        await refresh();
      });
      notice = undoingMoves ? "Reorganization undone. Original folder history is preserved." : "Archive reorganized. Original folder history is preserved.";
    });
  }

  async function restore(item: Item) {
    await run(async () => {
      await mutate(async () => { await api("restore_item", { id: item.id }); selected = null; await refresh(true); });
    });
  }

  async function purge(item: Item) {
    await run(async () => {
      if (!await confirm(`Permanently delete this archive undo copy: ${item.originalName}?\n\nIt may be the LAST byte-identical original, especially if its source was cleaned up. The better-looking keeper is not an exact backup. This cannot be undone.`, { title: "Permanently purge original", kind: "warning" })) return;
      await mutate(async () => {
        await api("purge_item", { id: item.id, acknowledgeOriginalLoss: true });
        selected = null;
        await refresh(true);
      });
    });
  }

  function closeDetails() {
    selected = null; comparing = null; currentReview = null; videoSource = ""; secondVideoSource = "";
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
    if (!loaded || !native || !snapshot.root) return;
    if (key === lastQuery) { libraryLoading = false; return; }
    const seq = ++searchSeq;
    libraryLoading = true;
    const timer = setTimeout(() => {
      if (seq !== searchSeq) return;
      void loadLibrary(false, seq)
        .catch((reason: unknown) => {
          if (seq !== searchSeq) return;
          items = []; months = []; month = ""; page = 0; lastQuery = "";
          libraryLoading = false;
          error = message(reason);
        });
    }, 220);
    return () => { clearTimeout(timer); searchSeq += 1; };
  });

  onMount(() => {
    native = isTauri();
    mode = native ? getCurrentWindow().label : new URLSearchParams(window.location.search).get("window") ?? "main";
    skipKeeperConfirmation = window.localStorage.getItem("photo-dedupe.skipKeeperConfirmation") === "true";
    if (mode === "viewer") tab = "library";
    if (!native) return;
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
        snapshot.progress = event.payload;
      }));
      register(await listen("library-changed", () => {
        if (mutating) return;
        void refresh(true).catch((reason: unknown) => { error = message(reason); });
      }));
      register(await listen<string>("quit-blocked", (event) => { error = event.payload; }));
      snapshot = await api<Snapshot>("snapshot");
      if (!snapshot.root && mode === "main") {
        const path = await api<string | null>("last_archive");
        if (path) {
          try {
            await mutate(async () => { snapshot = await api<Snapshot>("open_archive", { path }); });
          } catch (reason: unknown) {
            notice = `Previous archive unavailable (${message(reason)}). Connect the disk or choose a folder.`;
          }
        }
      }
      config = { ...snapshot.settings };
      if (mode === "main") tools = await api<ToolStatus[]>("tool_status");
      await refresh();
      loaded = true;
    });
    return () => { disposed = true; for (const remove of removers) remove(); };
  });
</script>

<svelte:head><title>Family Archive</title><meta name="description" content="A safe home for family photos and videos" /></svelte:head>
<svelte:window onkeydown={viewerKey} />

{#snippet datePicker()}
  {#if months.length}
    <div class="date-picker">
      <label>Year
        <select aria-label="Choose year" value={year} disabled={working || libraryLoading} onchange={(event) => {
          const value = event.currentTarget.value;
          const entry = months.find((candidate) => value === "unknown" ? candidate.month === "unknown" : candidate.month.startsWith(`${value}-`));
          if (entry) void changeMonth(entry.month);
        }}>
          {#each years as value}<option value={value}>{value === "unknown" ? "Unknown date" : value}</option>{/each}
        </select>
      </label>
      {#if year !== "unknown"}
        <label>Month
          <select aria-label="Choose month" value={month} disabled={working || libraryLoading} onchange={(event) => changeMonth(event.currentTarget.value)}>
            {#each months.filter((entry) => entry.month.startsWith(`${year}-`)) as entry}
              <option value={entry.month}>{entry.month.slice(5, 7)} ({entry.count.toLocaleString()})</option>
            {/each}
          </select>
        </label>
      {/if}
      <span>{monthCount.toLocaleString()} memories</span>
    </div>
  {/if}
{/snippet}

{#snippet pageControls()}
  {#if monthCount > pageSize}
    <div class="page-controls">
      <button class="secondary" disabled={working || libraryLoading || page === 0} onclick={() => changePage(page - 1)}>Previous page</button>
      <span>Page {page + 1} of {Math.ceil(monthCount / pageSize)}</span>
      <button class="secondary" disabled={working || libraryLoading || (page + 1) * pageSize >= monthCount} onclick={() => changePage(page + 1)}>Next page</button>
    </div>
  {/if}
{/snippet}

{#if mode === "drop"}
  <main class="mini" class:dropping>
    <div class="mini-heading"><span class="brand-symbol">FA</span><strong>Family Archive</strong></div>
    <button class="mini-drop" disabled={!native} onclick={() => snapshot.root ? importFolders() : chooseArchive()}>
      <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M12 3v12m-5-5 5 5 5-5" /><path d="M4 15v5h16v-5" /></svg>
      <strong>{dropping ? "Drop to copy safely" : "Drop photo folders here"}</strong>
      <small>{snapshot.root ? snapshot.root.split(/[\\/]/).pop() : "Choose an archive first"}</small>
    </button>
    <div class="mini-bottom"><small>{busy ? `${snapshot.progress.status} ${percent}%` : "Originals kept safe"}</small><button class="text-button" onclick={() => run(async () => { await api("show_window", { kind: "main" }); })}>Open app</button></div>
    <label class="mini-pin"><input type="checkbox" bind:checked={onTop} onchange={() => run(async () => { await getCurrentWindow().setAlwaysOnTop(onTop); })} /> Always on top</label>
    {#if error}<p class="mini-error">{error}</p>{/if}
  </main>
{:else if mode === "viewer"}
  <main class="viewer-shell">
    <header class="viewer-header">
      <div><strong>Family Archive Viewer</strong><small>{snapshot.root?.split(/[\\/]/).pop() ?? "No archive open"}</small></div>
      <button class="secondary" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "main" }); })}>Open main app</button>
    </header>
    {#if error}<div class="banner error" role="alert">{error}</div>{/if}
    {#if !native}<div class="banner warning">Photo previews are available in the desktop app.</div>{/if}
    {#if selected}
      <div class="viewer-navigation">
        <button class="secondary" disabled={working} onclick={closeDetails}>Back to thumbnails</button>
        <span>{selected.item.originalName}</span>
        <div class="button-row">
          <button class="secondary" disabled={working || viewerIndex < 0 || page * pageSize + viewerIndex <= 0} onclick={() => navigatePhoto(-1)}>Previous</button>
          <button class="secondary" disabled={working || viewerIndex < 0 || page * pageSize + viewerIndex >= monthCount - 1} onclick={() => navigatePhoto(1)}>Next</button>
        </div>
      </div>
      <section class="viewer-stage" aria-label={selected.item.originalName}>
        {#if videoSource}
          <video src={videoSource} controls onerror={() => error = "This video proxy could not be played. Check the bundled media tools."}><track kind="captions" /></video>
        {:else}
          <Preview item={selected.item} large />
        {/if}
      </section>
      <div class="viewer-caption">
        <div><strong>{selected.item.media.date?.slice(0, 10) ?? "Date unknown"}</strong><small>{selected.item.albums.join(" / ")}</small><small>{selected.item.media.dateSource} / {selected.item.media.width} x {selected.item.media.height} / {bytes(selected.item.media.bytes)}</small></div>
        {#if selected.item.media.kind === "video" && !videoSource}<button class="secondary" disabled={working} onclick={() => selected && playVideo(selected.item)}>Play video</button>{/if}
        <small>Arrow keys to browse / Escape for thumbnails</small>
      </div>
      {#if selected.item.media.warnings.length}<details><summary>Photo information</summary>{#each selected.item.media.warnings as warning}<p class="muted">{warning}</p>{/each}</details>{/if}
    {:else}
      <div class="library-toolbar">
        <input class="search" aria-label="Search photos and original folders" bind:value={search} placeholder="Search photos, dates or original folders..." />
        <select aria-label="Filter viewer" bind:value={filter}><option value="all">All memories</option><option value="video">Videos</option><option value="unknown">Unknown dates</option><option value="estimated">Estimated dates</option></select>
      </div>
      {@render datePicker()}
      {#if items.length}
        <div class="photo-grid">{#each items as item (item.id)}<button class="photo-card" disabled={working || libraryLoading} onclick={() => selectItem(item.id)}><Preview {item} /><div class="photo-info"><strong>{item.originalName}</strong><span>{item.media.date?.slice(0, 10) ?? "Date unknown"}</span><small>{item.albums.join(" / ")}</small></div></button>{/each}</div>
        {@render pageControls()}
      {:else}
        <div class="empty"><h2>{snapshot.root ? "No photos to show yet." : "Open your archive in the main app."}</h2><p>{search ? "No matching memories. Try another search." : "Imported photos and videos will appear here."}</p></div>
      {/if}
    {/if}
  </main>
{:else}
  <div class="app-shell">
    <aside class="sidebar">
      <div class="brand"><span class="brand-symbol"><svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="m3 19 6-10 4 5 3-8 5 13H3Z" /><circle cx="7" cy="5" r="1.5" /></svg></span><div><strong>Family Archive</strong><small>Every memory, together.</small></div></div>
      <div class="nav-caption">YOUR ARCHIVE</div>
      <nav aria-label="Main navigation">
        <button class:active={tab === "home"} onclick={() => tab = "home"}><span class="nav-icon">01</span> Overview</button>
        <button class:active={tab === "library"} onclick={() => tab = "library"}><span class="nav-icon">02</span> Photo library <span class="nav-count">{snapshot.activeCount || ""}</span></button>
        <button class:active={tab === "review"} onclick={() => tab = "review"}><span class="nav-icon">03</span> Similarity review {#if reviews.length}<span class="nav-count">{reviews.length}</span>{/if}</button>
        <button class:active={tab === "settings"} onclick={() => { config = { ...snapshot.settings }; tab = "settings"; }}><span class="nav-icon">04</span> Settings</button>
      </nav>
      <div class="sidebar-bottom">
        <div class="safety-note"><span class="safe-dot"></span><strong>Local. Private. Yours.</strong><p>Your photos never leave your disks. Importing doesn't change the originals.</p></div>
        <button class="secondary full" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "drop" }); })}>Open floating drop window</button>
        <button class="text-button full" disabled={!native} onclick={() => run(async () => { await api("show_window", { kind: "viewer" }); })}>Open separate viewer</button>
        <small class="version">FAMILY ARCHIVE / 0.1</small>
      </div>
    </aside>

    <main class="content">
      <header class="topbar"><span>{tab === "home" ? "Your collection, simplified" : tab === "library" ? "Browse your memories" : tab === "review" ? "You're in control" : "Make it your own"}</span><div class="status"><span class="safe-dot"></span>{busy ? "Working on your archive" : "Offline by design"}</div></header>
      {#if !native}<div class="banner warning"><strong>Desktop preview</strong> Folder imports and photo previews work in the Tauri desktop app. Run <code>npm run desktop</code> to use them.</div>{/if}
      {#if error}<div class="banner error" role="alert"><span>{error}</span><button class="text-button" onclick={() => error = ""}>Dismiss</button></div>{/if}
      {#if notice}<div class="banner success" role="status">{notice}</div>{/if}
      {#if snapshot.progress.errors.length}<details class="banner error"><summary>{snapshot.progress.errors.length} import/recovery issue(s) need attention</summary>{#each snapshot.progress.errors as issue}<p>{issue}</p>{/each}</details>{/if}

      {#if tab === "home"}
        <div class="page-heading"><div><p class="eyebrow">LESS CLUTTER. MORE MEMORIES.</p><h1>A home for every memory.</h1><p class="subtitle">Bring scattered folders together. We'll organize the dates<br class="wide-only" /> and help you keep the best copies.</p></div><span class="hero-mark" aria-hidden="true"><svg width="60" height="60" viewBox="0 0 60 60" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="7" y="12" width="37" height="35" rx="4" transform="rotate(-10 7 12)" /><rect x="15" y="10" width="37" height="35" rx="4" fill="#f5f3ec" /><circle cx="26" cy="21" r="3" /><path d="m16 39 12-12 8 8 7-7 8 11" /></svg></span></div>
        <section class="destination">
          <span class="disk-icon"><svg width="25" height="25" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><rect x="3" y="6" width="18" height="13" rx="3" /><path d="M3 14h18M7 17h1m3 0h1" /></svg></span>
          <div><small>ARCHIVE DESTINATION</small><strong>{snapshot.root ?? "Choose a folder on your external disk"}</strong><span>{snapshot.root ? "Photos stay as ordinary files, with original folder history." : "A dedicated archive folder keeps things tidy."}</span></div>
          <button class="secondary" disabled={!native || busy} onclick={chooseArchive}>{snapshot.root ? "Change" : "Choose folder"}</button>
        </section>
        <button class="drop-zone" class:dropping disabled={!native} onclick={() => importFolders()}>
          <span class="drop-icon"><svg width="35" height="35" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><path d="M3 8V5h7l2 3h9v12H3V8Z" /><path d="M12 10v7m-3-3 3 3 3-3" /></svg></span>
          <h2>{dropping ? "Ready. Drop your folders." : "Drop photo folders here"}</h2><p>Or click to choose folders from your computer.</p><span class="primary">Choose source folders <span aria-hidden="true">+</span></span>
          <small>PHOTOS, RAW FILES & VIDEOS <span> / </span> COPIED, NEVER MOVED DURING IMPORT</small>
        </button>
        {#if busy || snapshot.progress.total > 0}
          <section class="progress-card"><div class="section-line"><strong>{snapshot.progress.status}</strong><span>{snapshot.progress.done} / {snapshot.progress.total} files</span></div><div class="progress-track"><div style={`width:${percent}%`}></div></div><p class="current-file">{snapshot.progress.current || `${snapshot.progress.copied} originals copied / ${snapshot.progress.duplicates} exact duplicates skipped`}</p>
            {#if busy}<div class="button-row"><button class="secondary" onclick={() => run(async () => { await api("control_import", { action: snapshot.progress.status === "paused" ? "resume" : "pause" }); })}>{snapshot.progress.status === "paused" ? "Resume" : "Pause"}</button><button class="text-button" onclick={() => run(async () => { await api("control_import", { action: "cancel" }); })}>Cancel import</button></div>{/if}
          </section>
        {/if}
        <div class="stats"><div class="stat"><small>MEMORIES TOGETHER</small><strong>{snapshot.activeCount.toLocaleString()}</strong><span>photos & videos in your archive</span></div><div class="stat"><small>ARCHIVE SIZE</small><strong>{bytes(snapshot.activeBytes)}</strong><span>verified original files</span></div><div class="stat"><small>SAFELY RETAINED</small><strong>{snapshot.undoCount.toLocaleString()}</strong><span>undo copies / {bytes(snapshot.undoBytes)}</span></div></div>
        <section class="next-steps"><div><h3>A little less clutter?</h3><p>Once copies are verified, review source files for cleanup.<br />Nothing leaves your source folders without your say-so.</p></div><button class="secondary" disabled={!native || !snapshot.root || busy} onclick={startCleanup}>Review source cleanup</button></section>
        {#if jobs.length}<section class="history"><div class="section-line"><h3>Recent imports</h3><small>Original folder names stay searchable</small></div>{#each jobs.slice(0, 5) as job}<div class="history-row"><div><strong>{job.roots.map((path) => path.split(/[\\/]/).pop()).join(", ")}</strong><small>{job.progress.status ?? "queued"} / {job.progress.copied ?? 0} copied / {job.progress.duplicates ?? 0} exact duplicates</small></div><button class="text-button" disabled={busy} onclick={() => importFolders(job.roots)}>Retry folders</button></div>{/each}</section>{/if}
      {:else if tab === "library"}
        <div class="page-heading"><div><p class="eyebrow">ALL THE GOOD MOMENTS</p><h1>Your photo library.</h1><p class="subtitle">Search a date, a filename, or an original folder like "Holiday Germany".</p></div><button class="primary" disabled={!native || !snapshot.root} onclick={() => importFolders()}>Add folders +</button></div>
        <div class="library-toolbar"><input class="search" aria-label="Search photos and original folders" bind:value={search} placeholder="Search memories or original folders..." /><select aria-label="Filter library" bind:value={filter}><option value="all">All memories</option><option value="video">Videos</option><option value="unknown">Unknown dates</option><option value="estimated">Estimated dates</option><option value="undo">Retained undo copies</option></select></div>
        {@render datePicker()}
        {#if items.length}
          <div class="photo-grid">{#each items as item (item.id)}<button class="photo-card" disabled={working || libraryLoading} onclick={() => selectItem(item.id)}><Preview {item} /><div class="photo-info"><strong>{item.originalName}</strong><span>{item.media.date?.slice(0, 10) ?? "Date unknown"}{item.media.dateSource.startsWith("estimated") ? " / estimated" : ""}</span><small>{item.albums.join(" / ")}</small></div></button>{/each}</div>
          {@render pageControls()}
        {:else}<div class="empty"><h2>{snapshot.root ? "Your memories will appear here." : "Start with an archive folder."}</h2><p>{search ? "No matches. Try another filename, date or original folder." : "Import folders from the overview to fill your library."}</p><button class="secondary" onclick={() => tab = "home"}>Go to overview</button></div>{/if}
      {:else if tab === "review"}
        <div class="page-heading"><div><p class="eyebrow">SIMILAR DOESN'T ALWAYS MEAN THE SAME</p><h1>A second look.</h1><p class="subtitle">Compare uncertain matches. Edits, bursts and RAW/JPEG pairs deserve to stay.</p></div><div class="review-actions"><span class="pill">{reviews.length} to review</span>{#if reviews.length}<button class="primary" disabled={working || busy} onclick={() => compare(reviews[0])}>Review next match</button>{/if}</div></div>
        <label class="check-setting review-confirm-setting"><input type="checkbox" bind:checked={skipKeeperConfirmation} onchange={updateSkipKeeperConfirmation} /><span><strong>Skip confirmation when keeping a copy</strong><small>Applies on this computer. The other original still goes to archive undo.</small></span></label>
        {#if reviews.length}<div class="review-list">{#each reviews as review}<div class="review-row"><div class="review-symbol">2</div><div><strong>Two similar memories</strong><p>{review.reason}</p></div><button class="secondary" disabled={working || busy} onclick={() => compare(review)}>Compare files</button></div>{/each}</div>{:else}<div class="empty"><span class="empty-check" aria-hidden="true">OK</span><h2>Nothing needs a second look.</h2><p>Uncertain matches will appear here. Exact duplicates are handled automatically.</p></div>{/if}
      {:else}
        <div class="page-heading"><div><p class="eyebrow">SIMPLE SETTINGS, YOUR WAY</p><h1>A place for everything.</h1><p class="subtitle">Archive settings travel with your disk. Changes apply to future imports.</p></div></div>
        <section class="settings-card"><h3>Folder organization</h3><label>Choose a layout<select bind:value={config.template}>{#each presets as [value, label]}<option {value}>{label}</option>{/each}{#if !presets.some(([value]) => value === config.template)}<option value={config.template}>Custom layout</option>{/if}</select></label><details class="advanced"><summary>Advanced folder template</summary><label>Template<input bind:value={config.template} placeholder={"Family/{yyyy}/{mm}"} /></label><p>Supported: <code>{"{yyyy}"}</code> <code>{"{mm}"}</code> <code>{"{dd}"}</code> <code>{"{media_type}"}</code>. Safe literal folder names are welcome.</p></details>
          <div class="setting-columns"><label>Filenames<select bind:value={config.filenames}><option value="original">Keep original filenames</option><option value="date">Use photo date / time</option></select></label><label>Unknown-date folder<input bind:value={config.unknownFolder} /></label></div>
          <div class="path-preview"><small>YOUR FILES WOULD LOOK LIKE THIS</small>{#each examples as example}<code>{example}</code>{/each}{#if !native}<code>2023/07/18/IMG_1234.jpg</code><code>unknown/undated.jpg</code>{/if}{#if configError}<p class="field-error">{configError}</p>{/if}<p>Name collisions receive a unique suffix. Estimated dates stay marked; missing times aren't invented.</p></div>
          <label class="check-setting"><input type="checkbox" bind:checked={config.automaticMatches} /><span><strong>Automatically group very confident matches</strong><small>Keep a clearly better equivalent copy active. All other originals remain in undo. Ambiguous variants go to review.</small></span></label>
          <div class="button-row"><button class="primary" disabled={!native || !snapshot.root || busy || !!configError} onclick={() => run(async () => { await mutate(async () => { await api("save_settings", { settings: config }); await refresh(true); }); notice = "Settings saved for future imports. Existing files haven't moved."; })}>Save archive settings</button><button class="text-button" onclick={() => config = { ...defaults }}>Reset defaults</button></div>
        </section>
        <section class="next-steps"><div><h3>Apply the saved layout to older imports?</h3><p>Preview existing file moves first. Sources and folder history stay untouched.</p></div><button class="secondary" disabled={!native || !snapshot.root || busy} onclick={previewMoves}>Preview reorganization</button></section>
        <button class="text-button" disabled={!native || !snapshot.root || busy} onclick={() => run(async () => { moves = await api<MoveRow[]>("rollback_preview"); undoingMoves = true; })}>Preview undo of last reorganization</button>
        <section class="settings-card"><h3>Media tools</h3><p class="muted">RAW and video formats depend on the bundled decoders. Unsupported originals can still be archived without a preview.</p><div class="tool-list">{#each tools as tool}<div><span class:unavailable={!tool.available} class="safe-dot"></span><strong>{tool.name}</strong><small title={tool.detail}>{tool.available ? "Available" : tool.detail}</small></div>{/each}</div></section>
      {/if}
      <footer>Made for memories, not more maintenance. <span>Sources stay safe until you explicitly clean them up.</span></footer>
    </main>
  </div>
{/if}

{#if selected && mode !== "viewer"}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-backdrop" onclick={(event) => { if (!working && event.target === event.currentTarget) closeDetails(); }}>
    <dialog open class="modal detail-modal" class:comparison={!!comparing} aria-modal="true" aria-label={comparing ? "Compare similar memories" : selected.item.originalName} tabindex="-1">
      <div class="modal-top"><div><small>{comparing ? "COMPARE THE ORIGINALS" : "YOUR MEMORY"}</small><h2>{comparing ? "Which copy would you keep?" : selected.item.originalName}</h2></div><button class="secondary" onclick={closeDetails}>Close</button></div>
      {#if error}<div class="banner error" role="alert">{error}</div>{/if}
      <div class="detail-columns">
        {#each [selected, ...(comparing ? [comparing] : [])] as detail, index (detail.item.id)}
          <div class="detail-column">
            <Preview item={detail.item} large />
            {#if detail.item.media.kind === "video"}
              {#if index === 0 && videoSource}<video bind:this={firstVideo} src={videoSource} controls onerror={() => error = "This video proxy could not be played. Check the bundled decoder/encoder."}><track kind="captions" /></video>{:else if index === 1 && secondVideoSource}<video bind:this={secondVideo} src={secondVideoSource} controls onerror={() => error = "The comparison video could not be played."}><track kind="captions" /></video>{:else}<button class="secondary full" disabled={working} onclick={() => playVideo(detail.item, index === 1)}>Prepare playable video preview</button>{/if}
            {/if}
            <h3>{detail.item.originalName}</h3><div class="detail-facts"><span>{detail.item.media.width} x {detail.item.media.height}</span><span>{bytes(detail.item.media.bytes)}</span><span>{detail.item.media.kind.toUpperCase()}</span></div><p class="detail-date">{detail.item.media.date ?? "Date unknown"} <small>{detail.item.media.dateSource}</small></p><code class="file-path">{detail.item.path}</code>
            {#if detail.companions.length}<p class="banner warning">Has associated RAW/JPEG, sidecar or Live Photo files. Keep the set together.</p>{/if}
            <details><summary>Original folders & import history</summary>{#each detail.sources as source}<p class="source-history"><strong>{source.album}</strong><code>{source.relative}</code><small>{source.status.startsWith("staged:") ? "Cleanup recovery needed" : source.status === "trashed" ? "Source moved to OS trash" : "Source retained"}</small></p>{/each}</details>
            {#if detail.item.media.warnings.length}<details><summary>Metadata & quality notes</summary>{#each detail.item.media.warnings as warning}<p class="muted">{warning}</p>{/each}</details>{/if}
            {#if comparing}<button class="primary full" disabled={working || !!detail.companions.length} onclick={() => resolve(detail.item.id)}>Keep this copy active</button>{/if}
          </div>
        {/each}
      </div>
      {#if comparing}
        <div class="modal-bottom"><p>{currentReview?.reason}</p><div class="button-row">{#if videoSource && secondVideoSource}<button class="secondary" onclick={() => run(async () => { if (firstVideo && secondVideo) { firstVideo.currentTime = 0; secondVideo.currentTime = 0; await Promise.all([firstVideo.play(), secondVideo.play()]); } })}>Play both from start</button><button class="secondary" onclick={() => { firstVideo?.pause(); secondVideo?.pause(); }}>Pause both</button>{/if}<button class="secondary" disabled={working} onclick={() => resolve(null)}>Keep both / dismiss match</button></div></div>
      {:else}
        <div class="modal-bottom">
          {#if selected.item.state === "undo"}<div class="button-row"><button class="primary" disabled={working} onclick={() => selected && restore(selected.item)}>Restore this original</button><button class="danger-button" disabled={working} onclick={() => selected && purge(selected.item)}>Permanently purge undo copy</button></div>{:else}<div class="date-edit"><label>Correct the date<input type="date" bind:value={dateEntry} /></label><button class="secondary" disabled={working || !dateEntry} onclick={() => run(async () => { if (!selected) return; const id = selected.item.id; await mutate(async () => { await api("set_date", { id, date: dateEntry }); closeDetails(); await refresh(true); }); })}>Save date & reorganize this file</button></div>{/if}
        </div>
      {/if}
    </dialog>
  </div>
{/if}

{#if cleanup !== null}
  <div class="modal-backdrop"><dialog open class="modal" aria-modal="true" aria-label="Source cleanup preview" tabindex="-1"><div class="modal-top"><div><small>EXPLICITLY VERIFIED, NEVER AUTOMATIC</small><h2>Clean up source folders.</h2></div><button class="secondary" disabled={working} onclick={() => cleanup = null}>Close</button></div><div class="banner warning">Only byte-identical archived originals qualify. Cleanup may leave one copy. Trash usually needs emptying before space is freed.</div>{#if error}<div class="banner error" role="alert">{error}</div>{/if}
    <div class="cleanup-list">{#each cleanup as row}<label class="cleanup-row"><input type="checkbox" bind:group={cleanupSelection} value={row.id} disabled={!row.eligible || working} /><span><strong>{row.source}</strong><small class:field-error={!row.eligible}>{row.reason}</small></span><small>{bytes(row.bytes)}</small></label>{/each}{#if !cleanup.length}<p class="muted">No pending imported sources to clean up.</p>{/if}</div><label class="check-setting"><input type="checkbox" bind:checked={emptyFolders} /><span><strong>Also remove genuinely empty imported subfolders</strong><small>No recursive deletion. Dropped roots, hidden files and nonempty folders remain.</small></span></label>{#each cleanupResults as result}<p class="cleanup-result">{result}</p>{/each}<div class="modal-bottom button-row"><strong>{cleanupSelection.length} selected / {bytes(cleanup.filter((row) => cleanupSelection.includes(row.id)).reduce((sum, row) => sum + row.bytes, 0))} to trash</strong><button class="primary" disabled={working || !cleanupSelection.length} onclick={cleanSources}>Confirm cleanup...</button></div></dialog></div>
{/if}

{#if moves !== null}
  <div class="modal-backdrop"><dialog open class="modal" aria-modal="true" aria-label="Archive reorganization preview" tabindex="-1"><div class="modal-top"><div><small>PREVIEW BEFORE MOVING</small><h2>Reorganize your archive.</h2></div><button class="secondary" disabled={working} onclick={() => moves = null}>Close</button></div>{#if error}<div class="banner error" role="alert">{error}</div>{/if}<p class="muted">Only managed active files move. Originals, undo copies and original-folder albums stay intact.</p><div class="move-list">{#each moves as move}<div><code>{move.from}</code><small>becomes</small><code>{move.to}</code></div>{/each}{#if !moves.length}<p>Everything already follows the saved layout.</p>{/if}</div><div class="modal-bottom button-row"><span>{moves.length} file(s) to reorganize</span><button class="primary" disabled={working || !moves.length} onclick={applyMoves}>Confirm reorganization...</button></div></dialog></div>
{/if}

<style>
  :global(*) { box-sizing: border-box; }
  :global(body) { margin: 0; background: #f5f3ed; color: #21372d; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; font-size: 14px; -webkit-font-smoothing: antialiased; }
  :global(button), :global(input), :global(select) { font: inherit; }
  :global(button) { cursor: pointer; }
  :global(button:disabled) { cursor: not-allowed; opacity: .48; }
  :global(button:focus-visible), :global(input:focus-visible), :global(select:focus-visible), :global(summary:focus-visible) { outline: 3px solid #c9a75b; outline-offset: 3px; }
  :global(input), :global(select) { border: 1px solid #d8ddd2; background: #fffefb; color: #243b2d; border-radius: 8px; padding: 11px 13px; min-width: 0; }
  :global(code) { font-size: 12px; overflow-wrap: anywhere; }
  :global(h1), :global(h2), :global(h3), :global(p) { margin-top: 0; }
  :global(h1) { font-family: Georgia, "Times New Roman", serif; font-size: 39px; line-height: 1.15; font-weight: 500; letter-spacing: -.9px; margin-bottom: 13px; }
  :global(h2) { font-size: 22px; letter-spacing: -.4px; }
  :global(h3) { font-size: 16px; margin-bottom: 8px; }
  :global(small) { font-size: 11px; }
  :global(details) { margin: 15px 0; }
  :global(summary) { cursor: pointer; font-size: 12px; font-weight: 600; }
  .app-shell { display: flex; min-height: 100vh; }
  .viewer-shell { min-height: 100vh; padding: 0 28px 24px; }
  .viewer-header { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 20px 0; margin-bottom: 20px; border-bottom: 1px solid #dfe3d8; }
  .viewer-header strong { display: block; font-size: 16px; }
  .viewer-header small { display: block; color: #85917c; margin-top: 6px; }
  .viewer-navigation { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 16px; }
  .viewer-navigation > span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .viewer-navigation .button-row { margin: 0; flex-wrap: nowrap; }
  .viewer-stage { background: #172721; border-radius: 12px; overflow: hidden; }
  .viewer-stage :global(.preview.large) { height: max(320px, calc(100vh - 260px)); }
  .viewer-stage video { display: block; margin: 0; height: max(320px, calc(100vh - 260px)); max-height: none; object-fit: contain; }
  .viewer-caption { display: flex; justify-content: space-between; align-items: center; gap: 20px; margin-top: 18px; }
  .viewer-caption strong, .viewer-caption small { display: block; }
  .viewer-caption small { color: #85917c; margin-top: 5px; }
  .sidebar { width: 235px; flex-shrink: 0; background: #fdfcf8; border-right: 1px solid #e2e5dc; display: flex; flex-direction: column; padding: 30px 19px 24px; position: sticky; top: 0; height: 100vh; }
  .brand { display: flex; align-items: center; gap: 10px; margin: 0 4px 48px; }
  .brand-symbol { display: grid; place-items: center; width: 41px; height: 41px; background: #274b38; color: #f2e9c9; border-radius: 11px; flex-shrink: 0; }
  .brand strong { display: block; font-size: 15px; letter-spacing: -.25px; }
  .brand small { display: block; margin-top: 4px; font-size: 10px; color: #8b9285; }
  .nav-caption, .eyebrow { font-size: 9px; font-weight: 700; letter-spacing: .14em; }
  .nav-caption { color: #9b9f93; margin: 0 13px 12px; }
  nav { display: flex; flex-direction: column; gap: 6px; }
  nav button { border: 0; background: transparent; color: #697664; text-align: left; padding: 13px 11px; display: flex; align-items: center; gap: 11px; font-size: 12px; border-radius: 8px; }
  nav button.active { color: #274b38; background: #eaf0e4; font-weight: 600; }
  .nav-icon { font-size: 9px; opacity: .65; width: 17px; }
  .nav-count { margin-left: auto; border-radius: 5px; background: #dde8d4; padding: 2px 6px; font-size: 10px; }
  .sidebar-bottom { margin-top: auto; padding-top: 20px; }
  .safety-note { padding: 16px 12px; margin-bottom: 15px; background: #f2f3eb; border-radius: 9px; }
  .safety-note strong { font-size: 11px; margin-left: 6px; }
  .safety-note p { font-size: 11px; line-height: 1.7; color: #858e7e; margin: 10px 0 0; }
  .safe-dot { display: inline-block; height: 6px; width: 6px; background: #5c8753; border-radius: 50%; }
  .version { display: block; color: #a6ac9d; font-size: 8px; text-align: center; letter-spacing: .13em; margin-top: 22px; }
  .content { flex: 1; min-width: 0; padding: 0 38px 20px; max-width: 1560px; }
  .topbar { height: 76px; display: flex; align-items: center; justify-content: space-between; border-bottom: 1px solid #e0e3d9; font-size: 11px; color: #87917f; }
  .status { display: flex; align-items: center; gap: 7px; font-size: 10px; }
  .page-heading { display: flex; justify-content: space-between; align-items: center; padding: 34px 0 24px; gap: 18px; }
  .eyebrow { color: #8e9b82; margin-bottom: 15px; }
  .subtitle { color: #7a8573; line-height: 1.7; font-size: 12px; margin-bottom: 0; }
  .hero-mark { color: #a9b79b; border: 1px solid #e4e7dd; padding: 18px; border-radius: 50%; margin-right: 12px; }
  .primary, .secondary, .danger-button, .text-button { display: inline-flex; align-items: center; justify-content: center; gap: 16px; border-radius: 8px; padding: 11px 17px; font-size: 11px; font-weight: 600; white-space: nowrap; }
  .primary { border: 1px solid #2a4e39; background: #2a4e39; color: #fffef6; box-shadow: 0 3px 6px #1d3c1b0f; }
  .primary:hover { background: #365f44; }
  .secondary { background: #fffdf8; border: 1px solid #d6dccf; color: #526747; }
  .secondary:hover { background: #f0f3e8; }
  .text-button { border: 0; background: transparent; color: #6d805e; padding: 10px; }
  .danger-button { border: 1px solid #e5ccc3; background: #fff9f6; color: #9d493b; }
  .full { width: 100%; }
  .destination { display: flex; align-items: center; gap: 14px; border: 1px solid #dfe3d8; background: #fffdf8; padding: 17px 19px; border-radius: 11px; margin-bottom: 19px; }
  .disk-icon { background: #f1f2e8; color: #83936d; padding: 11px; border-radius: 9px; }
  .destination div { flex: 1; min-width: 0; }
  .destination small { color: #939c87; font-size: 8px; font-weight: 700; letter-spacing: .12em; }
  .destination strong { display: block; font-size: 12px; margin: 6px 0 5px; overflow-wrap: anywhere; }
  .destination span:not(.disk-icon) { color: #939c87; font-size: 10px; }
  .drop-zone { width: 100%; border: 1.5px dashed #c7d4ba; background: #eff2e7; padding: 27px 20px 25px; border-radius: 13px; color: #3c5b31; display: flex; align-items: center; flex-direction: column; }
  .drop-zone:disabled { opacity: 1; }
  .drop-zone:hover, .dropping { background: #e5ecd8 !important; border-color: #6d9056 !important; }
  .drop-icon { display: grid; place-items: center; width: 63px; height: 63px; background: #e4ebd8; border-radius: 16px; margin-bottom: 17px; }
  .drop-zone h2 { font-family: Georgia, serif; font-size: 25px; font-weight: 500; margin-bottom: 9px; }
  .drop-zone p { color: #829375; font-size: 12px; margin-bottom: 21px; }
  .drop-zone small { color: #95a18a; font-size: 8px; letter-spacing: .08em; margin-top: 22px; }
  .drop-zone small span { margin: 0 9px; color: #b7c0ad; }
  .stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 15px; margin: 19px 0; }
  .stat { padding: 19px 21px; background: #fffdf8; border: 1px solid #e0e4d9; border-radius: 10px; }
  .stat small { color: #939e86; font-size: 8px; font-weight: 700; letter-spacing: .1em; }
  .stat strong { display: block; font-size: 30px; font-weight: 500; letter-spacing: -1px; margin: 9px 0 5px; }
  .stat span { font-size: 10px; color: #919b87; }
  .next-steps { display: flex; justify-content: space-between; align-items: center; gap: 18px; padding: 21px 23px; border-radius: 11px; border: 1px solid #e0e3d8; background: #f9f8f1; margin: 20px 0; }
  .next-steps p { color: #8a957f; font-size: 11px; line-height: 1.7; margin: 0; }
  .next-steps h3 { font-size: 14px; }
  .banner { padding: 13px 16px; border-radius: 8px; font-size: 12px; margin: 16px 0; line-height: 1.6; overflow-wrap: anywhere; }
  .banner.error { color: #984536; background: #faeae3; }
  div.banner.error { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .banner.warning { color: #8b6b2a; background: #f5ebcf; }
  .banner.success { color: #406239; background: #e8efdc; }
  details.banner p { font-size: 11px; }
  .progress-card { border: 1px solid #dce3d4; background: #fffdf8; padding: 19px; border-radius: 10px; margin-top: 18px; }
  .section-line { display: flex; justify-content: space-between; align-items: center; gap: 12px; font-size: 11px; }
  .section-line strong { text-transform: capitalize; }
  .section-line span, .section-line small { color: #929c85; }
  .progress-track { height: 5px; background: #e9eddf; margin: 13px 0 10px; border-radius: 5px; overflow: hidden; }
  .progress-track div { height: 100%; background: #6e8d54; transition: width .2s; }
  .current-file { font-size: 10px; color: #8a9680; overflow-wrap: anywhere; margin-bottom: 0; }
  .button-row { display: flex; align-items: center; gap: 10px; margin-top: 17px; flex-wrap: wrap; }
  .history { margin-top: 26px; }
  .history-row { display: flex; justify-content: space-between; align-items: center; border-bottom: 1px solid #e1e5d9; padding: 13px 0; }
  .history-row strong { display: block; font-size: 12px; margin-bottom: 5px; }
  .history-row small { color: #8c987f; }
  .library-toolbar { display: flex; gap: 13px; margin: 0 0 25px; }
  .date-picker { display: flex; align-items: end; flex-wrap: wrap; gap: 14px; margin: 0 0 22px; color: #526747; font-size: 12px; }
  .date-picker label { display: grid; gap: 5px; }
  .date-picker span { padding-bottom: 12px; }
  .page-controls { display: flex; justify-content: center; align-items: center; flex-wrap: wrap; gap: 16px; margin: 25px 0; color: #526747; font-size: 12px; }
  .search { flex: 1; }
  .photo-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 18px; }
  .photo-card { border: 1px solid #e0e3d8; padding: 8px; border-radius: 12px; background: #fffdf8; text-align: left; min-width: 0; color: #334c2e; }
  .photo-card:hover { border-color: #8aa371; box-shadow: 0 5px 20px #263d2210; }
  .photo-info { padding: 12px 6px 7px; }
  .photo-info strong, .photo-info span, .photo-info small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .photo-info strong { font-size: 12px; font-weight: 600; }
  .photo-info span { font-size: 10px; color: #879779; margin: 6px 0; }
  .photo-info small { font-size: 9px; color: #a1aa96; }
  .empty { padding: 80px 20px; text-align: center; color: #748468; background: #f0f2e9; border-radius: 13px; border: 1px dashed #d8dfcd; }
  .empty h2 { font-family: Georgia, serif; font-weight: 400; color: #4a6641; margin-bottom: 12px; }
  .empty p { font-size: 12px; }
  .empty-check { display: inline-grid; place-items: center; background: #e0ebd4; border-radius: 50%; width: 50px; height: 50px; margin-bottom: 19px; font-size: 11px; }
  .pill { border-radius: 30px; background: #e6eddc; padding: 9px 14px; color: #6b8654; font-size: 11px; white-space: nowrap; }
  .review-actions { display: flex; align-items: center; gap: 12px; }
  .review-confirm-setting { margin: 0 0 20px; }
  .review-list { display: flex; flex-direction: column; gap: 14px; }
  .review-row { display: flex; align-items: center; gap: 19px; padding: 23px; background: #fffdf8; border: 1px solid #dfe4d6; border-radius: 12px; }
  .review-row div:nth-child(2) { flex: 1; }
  .review-row strong { font-size: 13px; }
  .review-row p { font-size: 11px; line-height: 1.6; color: #8b977f; margin: 7px 0 0; }
  .review-symbol { width: 43px; height: 43px; display: grid; place-items: center; background: #ecf0e2; color: #8aa071; border-radius: 10px; font-family: Georgia, serif; font-size: 26px; }
  .settings-card { background: #fffdf8; border: 1px solid #dfe3d6; border-radius: 12px; padding: 27px; margin-bottom: 20px; }
  .settings-card h3 { margin-bottom: 21px; }
  .settings-card label { display: flex; flex-direction: column; gap: 8px; font-size: 11px; color: #788a69; }
  .setting-columns { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; margin-top: 21px; }
  .advanced { padding: 13px 0; border-bottom: 1px solid #e3e7da; }
  .advanced label { margin-top: 17px; }
  .advanced p { font-size: 11px; color: #8c997f; margin: 11px 0 0; line-height: 1.7; }
  .path-preview { background: #eff2e7; padding: 19px; border-radius: 8px; margin: 22px 0; display: flex; flex-direction: column; gap: 9px; }
  .path-preview small { font-size: 8px; letter-spacing: .11em; font-weight: 600; color: #8e9c7f; margin-bottom: 5px; }
  .path-preview code { color: #5f7850; font-size: 11px; }
  .path-preview p { font-size: 10px; color: #8b9a7c; margin: 9px 0 0; }
  .check-setting { display: flex !important; flex-direction: row !important; align-items: flex-start; gap: 10px !important; margin: 18px 0; }
  .check-setting input { accent-color: #496f3c; margin-top: 2px; }
  .check-setting strong, .check-setting small { display: block; }
  .check-setting strong { font-size: 12px; color: #4e6941; }
  .check-setting small { font-size: 10px; line-height: 1.6; color: #93a082; margin-top: 5px; }
  .muted { color: #8a997c; font-size: 11px; line-height: 1.7; }
  .field-error { color: #ac5944 !important; }
  .tool-list { display: flex; flex-direction: column; gap: 12px; margin-top: 20px; }
  .tool-list div { display: flex; align-items: center; gap: 10px; }
  .tool-list strong { font-size: 11px; min-width: 95px; }
  .tool-list small { color: #9ba48f; overflow-wrap: anywhere; }
  .unavailable { background: #c88152; }
  footer { font-size: 9px; color: #a7af9a; padding-top: 27px; display: flex; justify-content: space-between; gap: 20px; }
  .modal-backdrop { position: fixed; inset: 0; background: #13261acf; z-index: 10; overflow: auto; display: flex; align-items: flex-start; justify-content: center; padding: 30px 22px; }
  .modal { position: relative; margin: 0; border: 0; color: inherit; width: min(760px, 100%); background: #fffdf8; border-radius: 15px; padding: 27px; box-shadow: 0 20px 100px #0003; max-height: calc(100vh - 60px); overflow: auto; }
  .detail-modal { width: min(660px, 100%); }
  .comparison { width: min(1140px, 100%); }
  .modal-top { display: flex; align-items: center; justify-content: space-between; gap: 20px; margin-bottom: 22px; }
  .modal-top small { font-size: 8px; letter-spacing: .12em; color: #92a17f; }
  .modal-top h2 { font-family: Georgia, serif; font-weight: 400; margin: 8px 0 0; }
  .detail-columns { display: grid; grid-template-columns: 1fr; gap: 24px; }
  .comparison .detail-columns { grid-template-columns: 1fr 1fr; }
  .detail-column { min-width: 0; }
  .detail-column h3 { margin-top: 18px; }
  .detail-facts { display: flex; gap: 15px; color: #8a9a79; font-size: 11px; }
  .detail-date { font-size: 12px; margin: 15px 0 10px; }
  .detail-date small { display: block; font-size: 10px; color: #97a48a; margin-top: 6px; }
  .file-path { font-size: 11px; color: #8c9d7a; }
  .source-history { margin-top: 12px; font-size: 11px; display: flex; flex-direction: column; gap: 5px; }
  .source-history code, .source-history small { font-size: 10px; color: #96a386; }
  .modal-bottom { border-top: 1px solid #e2e6d8; padding-top: 20px; margin-top: 21px; }
  .modal-bottom p { font-size: 11px; color: #8a997b; line-height: 1.7; }
  .date-edit { display: flex; align-items: flex-end; gap: 15px; }
  .date-edit label { display: flex; flex-direction: column; gap: 8px; color: #859773; font-size: 11px; }
  video { width: 100%; max-height: 340px; margin-top: 13px; background: #182b1c; border-radius: 8px; }
  .cleanup-list, .move-list { max-height: 350px; overflow: auto; }
  .cleanup-row { display: flex; align-items: flex-start; gap: 12px; border-bottom: 1px solid #e2e6d8; padding: 15px 0; }
  .cleanup-row input { accent-color: #466b3c; }
  .cleanup-row span { flex: 1; min-width: 0; }
  .cleanup-row strong { display: block; font-size: 11px; overflow-wrap: anywhere; font-weight: 500; }
  .cleanup-row small { color: #95a486; font-size: 10px; display: block; margin-top: 5px; }
  .cleanup-result { font-size: 11px; color: #7f916d; overflow-wrap: anywhere; }
  .move-list div { display: flex; flex-direction: column; gap: 7px; padding: 15px 0; border-bottom: 1px solid #e2e6d8; }
  .move-list small { color: #9ba88e; }
  .mini { min-height: 100vh; background: #f8f8ef; padding: 15px; }
  .mini-heading { display: flex; gap: 9px; align-items: center; margin-bottom: 12px; font-size: 12px; }
  .mini .brand-symbol { width: 28px; height: 28px; border-radius: 8px; font-size: 8px; }
  .mini-drop { display: flex; flex-direction: column; align-items: center; width: 100%; padding: 20px 10px; border: 1px dashed #b7ca9f; border-radius: 10px; background: #eef3e3; color: #567447; gap: 10px; }
  .mini-drop strong { font-size: 12px; }
  .mini-drop small { font-size: 10px; color: #8b9d78; }
  .mini-bottom { display: flex; justify-content: space-between; align-items: center; margin-top: 4px; color: #8c9e78; }
  .mini-bottom .text-button { font-size: 10px; }
  .mini-bottom small, .mini-pin { font-size: 9px; }
  .mini-pin { display: flex; align-items: center; gap: 6px; color: #8b9c7a; }
  .mini-pin input { accent-color: #536f40; }
  .mini-error { font-size: 10px; color: #a65b45; overflow-wrap: anywhere; margin-top: 11px; }
  @media (max-width: 1000px) { .sidebar { width: 204px; padding: 25px 13px; } .content { padding: 0 25px 20px; } .hero-mark { display: none; } :global(h1) { font-size: 33px; } footer span { display: none; } }
  @media (max-width: 760px) { .sidebar { width: 170px; padding: 20px 9px; } .brand small, .safety-note, .nav-icon { display: none; } .brand { margin: 0 3px 30px; } .brand strong { font-size: 12px; } .content { padding: 0 18px 20px; } .stats { gap: 8px; } .stat { padding: 14px 12px; } .stat strong { font-size: 23px; } .next-steps { align-items: flex-start; flex-direction: column; } .comparison .detail-columns { grid-template-columns: 1fr; } .setting-columns { grid-template-columns: 1fr; } .wide-only { display: none; } }
</style>
