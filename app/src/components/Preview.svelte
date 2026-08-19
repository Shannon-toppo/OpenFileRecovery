<script lang="ts">
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import { preview } from "../lib/api";
  import type { ApiError, PreviewDto } from "../lib/types";

  let {
    session,
    index,
    title,
    subtitle = "",
  }: { session: number | null; index: number | null; title: string; subtitle?: string } = $props();

  let data = $state<PreviewDto | null>(null);
  let error = $state<ApiError | null>(null);
  let loading = $state(false);

  // 中身が本当に残っているかを目で確かめられるかで信頼感が決まる (PLAN.md 7章 4)。
  $effect(() => {
    const s = session;
    const i = index;
    data = null;
    error = null;
    if (s === null || i === null) return;
    loading = true;
    let cancelled = false;
    preview(s, i, 0)
      .then((p) => {
        if (!cancelled) data = p;
      })
      .catch((e) => {
        if (!cancelled) error = e as ApiError;
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  let url = $derived(data ? `data:${data.mime};base64,${data.data}` : "");
  let isImage = $derived(data?.mime.startsWith("image/") ?? false);
  let isText = $derived(data?.mime.startsWith("text/") ?? false);
  let text = $derived(isText && data ? decode(data.data) : "");

  function decode(base64: string): string {
    try {
      const raw = atob(base64);
      const buf = Uint8Array.from(raw, (c) => c.charCodeAt(0));
      return new TextDecoder().decode(buf).slice(0, 4000);
    } catch {
      return "";
    }
  }
</script>

<div class="col" style="gap: 8px">
  <div class="col" style="gap: 2px">
    <b class="truncate">{title}</b>
    {#if subtitle}<span class="muted mono truncate">{subtitle}</span>{/if}
  </div>

  <div class="frame">
    {#if index === null}
      <span class="muted">{$t("results.previewNone")}</span>
    {:else if loading}
      <span class="muted">{$t("common.loading")}</span>
    {:else if error}
      <span class="muted">{$t("results.previewFailed")}</span>
    {:else if data && isImage}
      <img src={url} alt={data.name} />
    {:else if data && isText}
      <pre class="mono">{text}</pre>
    {:else}
      <span class="muted">{$t("results.previewUnsupported")}</span>
    {/if}
  </div>

  {#if data}
    <div class="row spread muted" style="font-size: 11.5px">
      <span>{bytes(data.bytes)}</span>
      {#if data.truncated}<span>{$t("results.previewTruncated")}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .frame {
    display: grid;
    place-items: center;
    min-height: 220px;
    max-height: 340px;
    padding: 10px;
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    overflow: auto;
  }

  img {
    max-width: 100%;
    max-height: 320px;
    object-fit: contain;
  }

  pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-all;
    align-self: flex-start;
    font-size: 11.5px;
  }
</style>
