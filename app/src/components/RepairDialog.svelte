<script lang="ts">
  import { untrack } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";

  import { onJobEvent, startJob } from "../lib/api";
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import type { ApiError, RepairReportDto } from "../lib/types";

  let { file = "", onClose }: { file?: string; onClose: () => void } = $props();

  // ダイアログを開いた時点のファイルを初期値にする (以後は手で直せる)。
  let input = $state(untrack(() => file));
  let reference = $state("");
  let running = $state(false);
  let report = $state<RepairReportDto | null>(null);
  let error = $state<ApiError | null>(null);
  let notes = $state<string[]>([]);

  // 修復元は絶対に書き換えない。出力は必ず別のファイルにする (PLAN.md 5.6)。
  let output = $derived(input ? suggestOutput(input) : "");

  function suggestOutput(path: string): string {
    const cut = path.lastIndexOf(".");
    return cut > path.lastIndexOf("/") && cut > path.lastIndexOf("\\")
      ? `${path.slice(0, cut)}-repaired${path.slice(cut)}`
      : `${path}-repaired`;
  }

  async function choose(kind: "input" | "reference") {
    const path = await open({ multiple: false, directory: false });
    if (typeof path !== "string") return;
    if (kind === "input") input = path;
    else reference = path;
  }

  async function run() {
    running = true;
    report = null;
    error = null;
    notes = [];
    try {
      const job = await startJob({
        kind: "repair",
        input,
        output,
        reference: reference || undefined,
      });
      // 修復は 1 ファイルで終わるので、このダイアログの中だけで面倒を見る。
      const unlisten = await onJobEvent((event) => {
        if (event.job !== job) return;
        if (event.event === "note") notes.push(event.message);
        if (event.event === "finished" && event.result.kind === "repair") {
          report = event.result;
          running = false;
          void unlisten();
        }
        if (event.event === "failed") {
          error = { code: event.code, message: event.message };
          running = false;
          void unlisten();
        }
      });
    } catch (e) {
      error = e as ApiError;
      running = false;
    }
  }
</script>

<div class="backdrop">
  <div class="panel col dialog">
    <div class="row spread">
      <h2>{$t("repair.title")}</h2>
      <button class="ghost" onclick={onClose}>{$t("common.close")}</button>
    </div>
    <span class="muted">{$t("repair.lead")}</span>

    <label class="col" style="gap: 4px">
      <span>{$t("repair.input")}</span>
      <div class="row">
        <input class="grow mono" bind:value={input} />
        <button onclick={() => choose("input")}>{$t("common.choose")}</button>
      </div>
    </label>

    <label class="col" style="gap: 4px">
      <span>{$t("repair.reference")}</span>
      <div class="row">
        <input class="grow mono" bind:value={reference} />
        <button onclick={() => choose("reference")}>{$t("common.choose")}</button>
      </div>
      <span class="muted">{$t("repair.referenceHint")}</span>
    </label>

    <div class="col" style="gap: 4px">
      <span>{$t("repair.output")}</span>
      <div class="mono muted truncate">{output || "-"}</div>
    </div>

    <div class="row">
      <button class="primary" disabled={!input || running} onclick={run}>
        {running ? $t("run.repair") : $t("repair.run")}
      </button>
    </div>

    {#if error}
      <div class="notice bad">{error.message}</div>
    {/if}

    {#each notes as note, i (i)}
      <div class="notice warn">{note}</div>
    {/each}

    {#if report}
      <div class="col" style="gap: 8px">
        <h3>{$t("repair.result")}</h3>
        <div class="row wrap" style="gap: 18px">
          <span class="stat">
            <b>{$t(`repair.status.${report.status}`)}</b><span>{$t("common.status")}</span>
          </span>
          <span class="stat">
            <b>{report.format}</b><span>{$t("common.name")}</span>
          </span>
          <span class="stat">
            <b>{bytes(report.outputSize)}</b><span>{$t("common.size")}</span>
          </span>
        </div>
        <div>
          <b>{$t("repair.verification")}:</b>
          {report.verificationDetail}
        </div>
        {#if report.fixes.length > 0}
          <div class="col" style="gap: 2px">
            <b>{$t("repair.fixes")}</b>
            <ul>
              {#each report.fixes as fix, i (i)}<li>{fix}</li>{/each}
            </ul>
          </div>
        {/if}
        {#if report.issues.length > 0}
          <div class="col" style="gap: 2px">
            <b>{$t("repair.issues")}</b>
            <ul>
              {#each report.issues as issue, i (i)}<li>{issue}</li>{/each}
            </ul>
          </div>
        {/if}
        {#if report.output}
          <div class="mono muted truncate">{report.output}</div>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    display: grid;
    place-items: center;
    padding: 24px;
    z-index: 10;
  }

  .dialog {
    width: min(680px, 100%);
    max-height: 90vh;
    overflow: auto;
  }

  ul {
    margin: 0;
    padding-left: 20px;
  }
</style>
