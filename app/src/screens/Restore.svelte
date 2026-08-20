<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";

  import { bytes, duration } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app, runJob, stopJob } from "../lib/state.svelte";
  import ErrorBox from "../components/ErrorBox.svelte";
  import ProgressPanel from "../components/ProgressPanel.svelte";
  import RepairDialog from "../components/RepairDialog.svelte";

  let dest = $state("");
  let flatten = $state(false);
  let repairing = $state<string | null>(null);

  let job = $derived(app.job);
  let result = $derived(job.result?.kind === "restore" ? job.result : null);

  async function chooseDest() {
    const path = await open({ directory: true, multiple: false });
    if (typeof path === "string") dest = path;
  }

  async function start() {
    if (app.session === null) return;
    await runJob({
      kind: "restore",
      session: app.session,
      entries: app.selection,
      dest,
      flatten,
    });
  }
</script>

<div class="col" style="gap: 16px; max-width: 860px">
  <h2>{$t("restore.title")}</h2>

  {#if !job.running && !result}
    <div class="panel col">
      <div class="muted">
        {$t("results.selected", {
          count: app.selection.length,
          size: bytes(app.selectionBytes),
        })}
      </div>

      <label class="col" style="gap: 4px">
        <span>{$t("restore.dest")}</span>
        <div class="row">
          <input class="grow mono" bind:value={dest} />
          <button onclick={chooseDest}>{$t("restore.choose")}</button>
        </div>
      </label>

      <div class="notice warn">{$t("restore.sameDiskWarning")}</div>

      <label class="row">
        <input type="checkbox" bind:checked={flatten} />
        <span>{$t("restore.flatten")}</span>
      </label>

      <div class="row">
        <button class="primary" disabled={!dest} onclick={start}>{$t("restore.run")}</button>
        <button class="ghost" onclick={() => (app.step = "results")}>{$t("common.back")}</button>
      </div>
    </div>
  {/if}

  {#if job.error}
    <ErrorBox error={job.error} />
    <div class="row">
      <button onclick={() => (job.error = null)}>{$t("common.retry")}</button>
    </div>
  {/if}

  {#if job.running}
    <div class="panel"><ProgressPanel progress={job.progress} kind={job.kind} cancelRequested={job.cancelRequested} /></div>
    <div class="row"><button class="danger" onclick={stopJob}>{$t("common.cancel")}</button></div>
  {/if}

  {#if result}
    <div class="panel col" style="gap: 14px">
      <h3>{$t("restore.result")}</h3>
      {#if job.outcome === "cancelled"}
        <div class="notice warn">{$t("run.cancelled")}</div>
      {/if}

      <div class="row wrap" style="gap: 18px">
        <span class="stat"><b>{result.summary.copied}</b><span>{$t("restore.copied")}</span></span>
        <span class="stat"><b>{result.summary.partial}</b><span>{$t("restore.partial")}</span></span>
        <span class="stat"><b>{result.summary.failed}</b><span>{$t("restore.failed")}</span></span>
        <span class="stat">
          <b>{bytes(result.summary.bytesWritten)}</b><span>{$t("restore.written")}</span>
        </span>
        <span class="stat">
          <b>{bytes(result.summary.bytesMissing)}</b><span>{$t("restore.missing")}</span>
        </span>
        <span class="stat">
          <b>{duration(result.summary.elapsedSecs)}</b><span>{$t("common.elapsed")}</span>
        </span>
      </div>

      <div class="row wrap">
        <button class="primary" onclick={() => revealItemInDir(result.summary.destination)}>
          {$t("restore.openDest")}
        </button>
        <button onclick={() => (repairing = "")}>{$t("restore.repair")}</button>
        {#if result.summary.reportJson}
          <span class="muted mono truncate">{result.summary.reportJson}</span>
        {/if}
      </div>

      {#if result.incomplete.length > 0}
        <div class="col" style="gap: 6px">
          <b>{$t("restore.repairHint")}</b>
          <div class="scroll" style="max-height: 260px">
            <table>
              <thead>
                <tr>
                  <th>{$t("common.path")}</th>
                  <th>{$t("restore.missing")}</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {#each result.incomplete as file (file.output)}
                  <tr>
                    <td class="mono truncate">{file.source}</td>
                    <td class="mono">{bytes(file.missing)}</td>
                    <td>
                      <button onclick={() => (repairing = file.output)}>{$t("restore.repair")}</button>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</div>

{#if repairing !== null}
  <RepairDialog file={repairing} onClose={() => (repairing = null)} />
{/if}
