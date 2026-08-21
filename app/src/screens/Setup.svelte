<script lang="ts">
  import { open, save } from "@tauri-apps/plugin-dialog";

  import { resumeAvailable } from "../lib/api";
  import { t } from "../lib/i18n";
  import { app, runJob } from "../lib/state.svelte";
  import type { FsChoice, JobRequest } from "../lib/types";

  // 吸い出し
  let imageOutput = $state("");
  let imageOverwrite = $state(false);
  /** 出力先に再開用の記録 (.map) があるか。 */
  let resumable = $state(false);

  // 出力先が決まったら、続きから再開できるかを調べて先に伝える。
  // 中断した吸い出しを「最初からやり直す」と誤解させないため。
  $effect(() => {
    const path = imageOutput;
    if (!path) {
      resumable = false;
      return;
    }
    let cancelled = false;
    resumeAvailable(path)
      .then((yes) => {
        if (!cancelled) resumable = yes;
      })
      .catch(() => {
        if (!cancelled) resumable = false;
      });
    return () => {
      cancelled = true;
    };
  });
  let retries = $state(3);
  let blockSize = $state(1 << 20);
  let unmount = $state(false);

  // 解析
  let fs = $state<FsChoice>("auto");
  let deleted = $state(true);
  let orphans = $state(app.mode === "formatted");

  // コピー
  let copyDest = $state("");
  let includeDeleted = $state(false);
  let onExisting = $state<"rename" | "skip" | "overwrite">("rename");

  // カービング
  let carveOutput = $state("");
  let align = $state(512);
  const allFormats = ["jpeg", "png", "gif", "heic", "mp4", "mov", "avi", "wav", "mp3", "zip", "pdf"];
  let formats = $state<string[]>([]);

  let ready = $derived(
    app.mode === "image"
      ? imageOutput !== ""
      : app.mode === "copy"
        ? copyDest !== ""
        : app.mode === "carve"
          ? carveOutput !== ""
          : true,
  );

  async function chooseImageOutput() {
    const path = await save({
      title: $t("image.output"),
      defaultPath: "recovered.img",
      filters: [{ name: "Disk image", extensions: ["img"] }],
    });
    if (typeof path === "string") {
      imageOutput = path;
      // 保存ダイアログが上書きの確認を済ませているので、そのまま進めてよい。
      imageOverwrite = true;
    }
  }

  async function chooseFolder(): Promise<string | null> {
    const path = await open({ directory: true, multiple: false });
    return typeof path === "string" ? path : null;
  }

  function toggleFormat(name: string) {
    formats = formats.includes(name) ? formats.filter((f) => f !== name) : [...formats, name];
  }

  async function start() {
    const source = app.source!.id;
    let request: JobRequest;
    switch (app.mode) {
      case "image":
        request = {
          kind: "image",
          source,
          output: imageOutput,
          retries,
          blockSize,
          unmount,
          overwrite: imageOverwrite,
        };
        break;
      case "copy":
        request = {
          kind: "copy",
          source,
          dest: copyDest,
          fs,
          includeDeleted,
          onExisting,
        };
        break;
      case "carve":
        request = {
          kind: "carve",
          source,
          output: carveOutput,
          align,
          formats,
        };
        break;
      default:
        request = { kind: "scan", source, fs, deleted, orphans };
    }
    app.step = "run";
    await runJob(request);
  }
</script>

<div class="col" style="gap: 16px; max-width: 760px">
  <h2>{$t(`mode.${app.mode}.title`)}</h2>
  <p class="muted" style="margin: -10px 0 0">{$t(`mode.${app.mode}.desc`)}</p>

  {#if app.mode === "image"}
    <div class="panel col">
      <label class="col" style="gap: 4px">
        <span>{$t("image.output")}</span>
        <div class="row">
          <input class="grow mono" bind:value={imageOutput} placeholder="recovered.img" />
          <button onclick={chooseImageOutput}>{$t("common.choose")}</button>
        </div>
      </label>
      <span class="muted">{$t("image.mapfileHint")}</span>
      {#if resumable}
        <div class="notice">{$t("image.resume")}</div>
      {/if}
      <label class="row">
        <input type="checkbox" bind:checked={imageOverwrite} />
        <span>{$t("image.overwrite")}</span>
      </label>
      <div class="notice warn">{$t("image.sameDiskWarning")}</div>

      <div class="row wrap" style="gap: 18px">
        <label class="row">
          <span>{$t("image.retries")}</span>
          <input type="number" min="0" max="20" bind:value={retries} style="width: 72px" />
        </label>
        <label class="row">
          <span>{$t("image.blockSize")}</span>
          <select bind:value={blockSize}>
            <option value={65536}>64 KiB</option>
            <option value={262144}>256 KiB</option>
            <option value={1048576}>1 MiB</option>
            <option value={4194304}>4 MiB</option>
          </select>
        </label>
        <label class="row">
          <input type="checkbox" bind:checked={unmount} />
          <span>{$t("image.unmount")}</span>
        </label>
      </div>
    </div>
  {:else if app.mode === "copy"}
    <div class="panel col">
      <label class="col" style="gap: 4px">
        <span>{$t("copy.dest")}</span>
        <div class="row">
          <input class="grow mono" bind:value={copyDest} />
          <button onclick={async () => (copyDest = (await chooseFolder()) ?? copyDest)}>
            {$t("common.choose")}
          </button>
        </div>
      </label>
      <div class="notice warn">{$t("restore.sameDiskWarning")}</div>

      <label class="row">
        <input type="checkbox" bind:checked={includeDeleted} />
        <span>{$t("copy.includeDeleted")}</span>
      </label>
      <label class="col" style="gap: 4px">
        <span>{$t("copy.onExisting")}</span>
        <select bind:value={onExisting}>
          <option value="rename">{$t("copy.existing.rename")}</option>
          <option value="skip">{$t("copy.existing.skip")}</option>
          <option value="overwrite">{$t("copy.existing.overwrite")}</option>
        </select>
      </label>
    </div>
  {:else if app.mode === "carve"}
    <div class="panel col">
      <label class="col" style="gap: 4px">
        <span>{$t("carve.output")}</span>
        <div class="row">
          <input class="grow mono" bind:value={carveOutput} />
          <button onclick={async () => (carveOutput = (await chooseFolder()) ?? carveOutput)}>
            {$t("common.choose")}
          </button>
        </div>
      </label>
      <div class="notice warn">{$t("restore.sameDiskWarning")}</div>

      <label class="col" style="gap: 4px">
        <span>{$t("carve.align")}</span>
        <div class="row">
          <select bind:value={align}>
            <option value={512}>512 B</option>
            <option value={4096}>4 KiB</option>
            <option value={16384}>16 KiB</option>
            <option value={32768}>32 KiB</option>
          </select>
          <span class="muted">{$t("carve.alignHint")}</span>
        </div>
      </label>

      <div class="col" style="gap: 6px">
        <span>{$t("carve.formats")}</span>
        <div class="row wrap" style="gap: 6px">
          <button class:primary={formats.length === 0} onclick={() => (formats = [])}>
            {$t("carve.allFormats")}
          </button>
          {#each allFormats as name (name)}
            <button class:primary={formats.includes(name)} onclick={() => toggleFormat(name)}>
              {name}
            </button>
          {/each}
        </div>
      </div>
      <span class="muted">{$t("carve.noNames")}</span>
    </div>
  {:else}
    <div class="panel col">
      <h3>{$t("scan.options")}</h3>
      <label class="row">
        <input type="checkbox" bind:checked={deleted} />
        <span>{$t("scan.deleted")}</span>
      </label>
      <label class="row">
        <input type="checkbox" bind:checked={orphans} />
        <span>{$t("scan.orphans")}</span>
      </label>
      <label class="col" style="gap: 4px">
        <span>{$t("scan.fs")}</span>
        <select bind:value={fs}>
          <option value="auto">{$t("scan.fsAuto")}</option>
          <option value="fat32">FAT32</option>
          <option value="exfat">exFAT</option>
        </select>
      </label>
    </div>
  {/if}

  {#if !app.source?.isImage && app.mode !== "image"}
    <div class="notice col" style="gap: 2px">
      <b>{$t("safety.liveDevice")}</b>
      <span class="muted">{$t("safety.imageFirst")}</span>
    </div>
  {/if}

  <div class="row">
    <button class="primary" disabled={!ready} onclick={start}>{$t("common.start")}</button>
    <button class="ghost" onclick={() => (app.step = "mode")}>{$t("common.back")}</button>
  </div>
</div>
