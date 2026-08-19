<script lang="ts">
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app } from "../lib/state.svelte";
  import type { Mode } from "../lib/state.svelte";

  const modes: { id: Mode; icon: string }[] = [
    { id: "deleted", icon: "🔍" },
    { id: "formatted", icon: "🗂" },
    { id: "image", icon: "💾" },
    { id: "copy", icon: "📁" },
    { id: "carve", icon: "🧩" },
  ];

  // イメージファイルを相手にしているときは、吸い出しは意味がない。
  let available = $derived(
    app.source?.isImage ? modes.filter((m) => m.id !== "image") : modes,
  );

  function choose(mode: Mode) {
    app.mode = mode;
    app.step = "setup";
  }
</script>

<div class="col" style="gap: 16px; max-width: 940px">
  <div>
    <h2>{$t("mode.title")}</h2>
    <p class="muted" style="margin: 4px 0 0">{$t("mode.lead")}</p>
  </div>

  {#if app.source}
    <div class="panel row spread">
      <div class="col" style="gap: 2px">
        <b>{app.source.label}</b>
        <span class="muted mono">
          {app.source.id}
          {#if app.source.size > 0}&nbsp;· {bytes(app.source.size)}{/if}
        </span>
      </div>
      <button class="ghost" onclick={() => (app.step = "devices")}>{$t("common.back")}</button>
    </div>
  {/if}

  <div class="grid">
    {#each available as mode (mode.id)}
      <button class="card col" onclick={() => choose(mode.id)}>
        <div class="row" style="gap: 10px">
          <span class="icon">{mode.icon}</span>
          <b>{$t(`mode.${mode.id}.title`)}</b>
        </div>
        <span class="muted">{$t(`mode.${mode.id}.desc`)}</span>
      </button>
    {/each}
  </div>
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
    gap: 12px;
  }

  .card {
    align-items: flex-start;
    text-align: left;
    background: var(--panel);
    padding: 16px;
    gap: 6px;
    height: 100%;
  }

  .icon {
    font-size: 20px;
  }
</style>
