<script lang="ts">
  import { bytes, duration, eta, percent, rate } from "../lib/format";
  import { t } from "../lib/i18n";
  import type { JobKind, ProgressDto } from "../lib/types";
  import RegionMap from "./RegionMap.svelte";

  let { progress, kind }: { progress: ProgressDto | null; kind: JobKind | null } = $props();

  let imaging = $derived(kind === "image");
</script>

<div class="col" style="gap: 12px">
  <div class="row spread">
    <b>{progress ? $t(`run.phase.${progress.phase}`) : $t("common.loading")}</b>
    <span class="mono">{percent(progress?.ratio ?? 0)}</span>
  </div>

  <div class="bar"><div style="width: {(progress?.ratio ?? 0) * 100}%"></div></div>

  {#if progress?.current}
    <div class="muted mono truncate">{progress.current}</div>
  {/if}

  <div class="row wrap" style="gap: 18px">
    {#if imaging}
      <span class="stat"><b>{bytes(progress?.rescued ?? 0)}</b><span>{$t("run.rescued")}</span></span>
      <span class="stat"><b>{bytes(progress?.bad ?? 0)}</b><span>{$t("run.bad")}</span></span>
      <span class="stat"><b>{bytes(progress?.pending ?? 0)}</b><span>{$t("run.pending")}</span></span>
    {:else if progress && progress.itemsTotal > 0}
      <span class="stat">
        <b>{progress.itemsDone} / {progress.itemsTotal}</b><span>{$t("common.files")}</span>
      </span>
      <span class="stat"><b>{bytes(progress.bytesDone)}</b><span>{$t("restore.written")}</span></span>
    {:else}
      <span class="stat"><b>{progress?.found ?? 0}</b><span>{$t("run.found")}</span></span>
      <span class="stat"><b>{bytes(progress?.position ?? 0)}</b><span>{$t("common.path")}</span></span>
    {/if}
    <span class="stat"><b>{rate(progress?.rate ?? 0)}</b><span>{$t("common.speed")}</span></span>
    <span class="stat"><b>{eta(progress?.etaSecs ?? null)}</b><span>{$t("common.remaining")}</span></span>
    <span class="stat">
      <b>{duration(progress?.elapsedSecs ?? 0)}</b><span>{$t("common.elapsed")}</span>
    </span>
    <span class="stat"><b>{progress?.errors ?? 0}</b><span>{$t("common.errors")}</span></span>
  </div>

  {#if imaging && progress && progress.map.length > 0}
    <RegionMap map={progress.map} total={progress.total} />
  {/if}
</div>
