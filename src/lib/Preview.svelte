<script lang="ts">
  import { isTauri } from "@tauri-apps/api/core";
  import { message, thumbnail, type PreviewItem } from "./api";

  let { item, large = false }: { item: PreviewItem; large?: boolean } = $props();
  let element = $state<HTMLDivElement>();
  let source = $state("");
  let error = $state("");
  let loading = $state(false);

  $effect(() => {
    const id = item.id;
    const size = large;
    const target = element;
    source = "";
    error = "";
    loading = false;
    let active = true;
    let requested = false;
    const controller = new AbortController();
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting) || requested) return;
      requested = true;
      observer.disconnect();
      if (!isTauri()) { error = "Open the desktop app for previews."; return; }
      loading = true;
      thumbnail(id, size, controller.signal)
        .then((value) => { if (active) source = value; })
        .catch((reason: unknown) => {
          if (!active || controller.signal.aborted) return;
          error = message(reason);
        })
        .finally(() => { if (active) loading = false; });
    });
    if (target) observer.observe(target);
    return () => { active = false; controller.abort(); observer.disconnect(); };
  });
</script>

<div bind:this={element} class:large class="preview">
  {#if source}
    <img src={source} alt={item.originalName} draggable="false" />
  {:else}
    <div class="placeholder">
      <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.3" aria-hidden="true">
        <rect x="3" y="3" width="18" height="18" rx="3" /><circle cx="8" cy="8" r="1.5" /><path d="m3 18 6-6 4 4 3-3 5 5" />
      </svg>
      <span>{error ? "Preview unavailable" : loading ? "Opening preview..." : item.media.kind.toUpperCase()}</span>
      {#if error}<small title={error}>{error}</small>{/if}
    </div>
  {/if}
  {#if item.media.kind === "video"}<span class="type">VIDEO</span>{/if}
  {#if item.media.kind === "raw"}<span class="type">RAW</span>{/if}
</div>

<style>
  .preview { height: 172px; position: relative; background: #e9e9e1; overflow: hidden; border-radius: 10px; display: grid; place-items: center; }
  .preview.large { height: 340px; background: #172721; }
  img { width: 100%; height: 100%; object-fit: cover; }
  .large img { object-fit: contain; }
  .placeholder { display: flex; flex-direction: column; align-items: center; gap: 8px; padding: 16px; color: #7a8276; text-align: center; }
  .placeholder span { font-size: 11px; letter-spacing: .05em; }
  small { font-size: 10px; max-width: 300px; max-height: 55px; overflow: auto; }
  .type { position: absolute; right: 9px; bottom: 9px; color: white; background: #152a20c9; border-radius: 5px; padding: 4px 7px; font-size: 9px; font-weight: 700; letter-spacing: .06em; }
</style>
