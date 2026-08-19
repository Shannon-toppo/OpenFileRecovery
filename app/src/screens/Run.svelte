<script lang="ts">
  import { revealItemInDir } from "@tauri-apps/plugin-opener";

  import { bytes, duration, percent } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app, resetJob, stopJob } from "../lib/state.svelte";
  import ErrorBox from "../components/ErrorBox.svelte";
  import ProgressPanel from "../components/ProgressPanel.svelte";

  let job = $derived(app.job);
  let result = $derived(job.result);

  // 解析が終わって中身があるなら、そのまま結果画面へ送る (1 本道の流れ)。
  $effect(() => {
    if (result?.kind === "scan" && result.entryCount > 0) {
      app.step = "results";
    }
  });

  function retry() {
    resetJob();
    app.step = "setup";
  }

  async function reveal(path: string) {
    try {
      await revealItemInDir(path);
    } catch {
      // フォルダを開けなくても復旧作業には影響しない。
    }
  }

  function scanTheImage() {
    if (result?.kind !== "image") return;
    app.source = {
      id: result.imagePath,
      label: result.imagePath.split(/[\\/]/).pop() ?? result.imagePath,
      size: result.total,
      isImage: true,
    };
    app.device = null;
    app.mode = "formatted";
    resetJob();
    app.step = "setup";
  }
</script>

<div class="col" style="gap: 16px; max-width: 860px">
  <h2>{$t(`run.${app.mode === "formatted" || app.mode === "deleted" ? "scan" : app.mode}`)}</h2>

  {#if job.error}
    <ErrorBox error={job.error} />
    <div class="row">
      <button class="primary" onclick={retry}>{$t("common.retry")}</button>
      <button class="ghost" onclick={() => (app.step = "mode")}>{$t("common.back")}</button>
    </div>
  {:else if job.running}
    <div class="panel">
      <ProgressPanel progress={job.progress} kind={job.kind} />
    </div>
    <div class="row">
      <button class="danger" onclick={stopJob}>{$t("common.cancel")}</button>
    </div>
  {:else if result}
    <div class="panel col" style="gap: 14px">
      {#if job.outcome === "cancelled"}
        <div class="notice warn">{$t("run.cancelled")}</div>
      {/if}

      {#if result.kind === "image"}
        <h3>{$t("image.result")}</h3>
        <div class="row wrap" style="gap: 18px">
          <span class="stat">
            <b>{bytes(result.rescued)}</b><span>{$t("run.rescued")}</span>
          </span>
          <span class="stat"><b>{bytes(result.bad)}</b><span>{$t("run.bad")}</span></span>
          <span class="stat">
            <b>{bytes(result.remaining)}</b><span>{$t("run.pending")}</span>
          </span>
          <span class="stat">
            <b>{percent(result.total > 0 ? result.rescued / result.total : 1)}</b>
            <span>{$t("run.rescued")}</span>
          </span>
          <span class="stat">
            <b>{duration(result.elapsedSecs)}</b><span>{$t("common.elapsed")}</span>
          </span>
        </div>
        <div class="mono muted truncate">{result.imagePath}</div>
        {#if !result.complete && !result.cancelled}
          <div class="notice warn">{$t("image.incomplete")}</div>
        {/if}
        <div class="row wrap">
          <button class="primary" onclick={scanTheImage}>{$t("image.analyzeImage")}</button>
          <button onclick={() => reveal(result.imagePath)}>{$t("common.openFolder")}</button>
        </div>
      {:else if result.kind === "scan"}
        <h3>{$t("results.title")}</h3>
        <p>{$t("results.empty")}</p>
        <span class="muted">{$t("results.emptyHint")}</span>
        <div class="row wrap">
          <button
            class="primary"
            onclick={() => {
              app.mode = "carve";
              resetJob();
              app.step = "setup";
            }}
          >
            {$t("results.carveInstead")}
          </button>
          <button onclick={retry}>{$t("common.retry")}</button>
        </div>
      {:else if result.kind === "carve"}
        <h3>{$t("carve.result")}</h3>
        <div class="row wrap" style="gap: 18px">
          <span class="stat">
            <b>{result.summary.found}</b><span>{$t("run.found")}</span>
          </span>
          <span class="stat">
            <b>{bytes(result.summary.bytesRecovered)}</b><span>{$t("common.size")}</span>
          </span>
          <span class="stat">
            <b>{duration(result.summary.elapsedSecs)}</b><span>{$t("common.elapsed")}</span>
          </span>
        </div>
        <span class="muted">{$t("carve.noNames")}</span>
        <div class="row wrap">
          {#if result.summary.found > 0}
            <button class="primary" onclick={() => (app.step = "results")}>
              {$t("steps.results")}
            </button>
          {/if}
          {#if result.summary.output}
            <button onclick={() => reveal(result.summary.output!)}>{$t("common.openFolder")}</button>
          {/if}
        </div>
      {:else if result.kind === "copy" || result.kind === "restore"}
        <h3>{$t(result.kind === "copy" ? "copy.result" : "restore.result")}</h3>
        <div class="row wrap" style="gap: 18px">
          <span class="stat">
            <b>{result.summary.copied}</b><span>{$t("restore.copied")}</span>
          </span>
          <span class="stat">
            <b>{result.summary.partial}</b><span>{$t("restore.partial")}</span>
          </span>
          <span class="stat">
            <b>{result.summary.failed}</b><span>{$t("restore.failed")}</span>
          </span>
          <span class="stat">
            <b>{bytes(result.summary.bytesWritten)}</b><span>{$t("restore.written")}</span>
          </span>
          <span class="stat">
            <b>{duration(result.summary.elapsedSecs)}</b><span>{$t("common.elapsed")}</span>
          </span>
        </div>
        {#if result.incomplete.length > 0}
          <div class="col" style="gap: 4px">
            <b>{$t("restore.missing")}</b>
            <div class="scroll" style="max-height: 180px">
              <table>
                <tbody>
                  {#each result.incomplete as file (file.source)}
                    <tr>
                      <td class="mono truncate">{file.source}</td>
                      <td class="mono">{bytes(file.missing)}</td>
                      <td class="muted">{file.error ?? ""}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </div>
        {/if}
        <div class="row wrap">
          <button class="primary" onclick={() => reveal(result.summary.destination)}>
            {$t("restore.openDest")}
          </button>
          {#if result.summary.reportText}
            <span class="muted mono truncate">{result.summary.reportText}</span>
          {/if}
        </div>
      {/if}

      {#if job.notes.length > 0}
        <div class="col" style="gap: 6px">
          {#each job.notes as note, i (i)}
            <div class="notice" class:warn={note.level === "warn"}>{note.message}</div>
          {/each}
        </div>
      {/if}
    </div>
  {:else}
    <div class="panel"><ProgressPanel progress={null} kind={job.kind} /></div>
  {/if}
</div>
