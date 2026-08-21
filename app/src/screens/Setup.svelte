<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";

  import { outputState } from "../lib/api";
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app, runJob } from "../lib/state.svelte";
  import type { FsChoice, JobRequest } from "../lib/types";

  // 吸い出し。
  //
  // 保存ダイアログは使わない。あれは「置き換えますか?」しか聞けないので、
  // 中断した吸い出しの続きをやりたいときに逆のことを尋ねてしまう。
  // フォルダと名前を別々に受け取り、何が起きるかはこちらで判断して伝える。
  let imageFolder = $state("");
  let imageName = $state("recovered.img");
  let imageOverwrite = $state(false);
  let output = $derived(imageFolder ? `${imageFolder}/${imageName}` : "");

  /** 出力先の状態 (既存か、続きから進めるか)。 */
  let dest = $state({ exists: false, resumable: false, rescued: 0, total: 0 });

  // 出力先が決まったら、上書きになるのか続きからになるのかを先に伝える。
  // 中断した吸い出しを「最初からやり直す」と誤解されると、壊れかけメディアを
  // 丸ごと読み直すことになる (PLAN.md 6章 4項)。
  $effect(() => {
    const path = output;
    if (!path) {
      dest = { exists: false, resumable: false, rescued: 0, total: 0 };
      return;
    }
    let cancelled = false;
    outputState(path)
      .then((state) => {
        if (cancelled) return;
        dest = state;
        // 続きからなら上書きの了承は要らない。取り直すときだけ聞く。
        if (state.resumable) imageOverwrite = false;
      })
      .catch(() => {});
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
      ? output !== "" && imageName !== "" && (!dest.exists || dest.resumable || imageOverwrite)
      : app.mode === "copy"
        ? copyDest !== ""
        : app.mode === "carve"
          ? carveOutput !== ""
          : true,
  );

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
          output,
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
        <span>{$t("image.folder")}</span>
        <div class="row">
          <input class="grow mono" bind:value={imageFolder} />
          <button onclick={async () => (imageFolder = (await chooseFolder()) ?? imageFolder)}>
            {$t("image.chooseFolder")}
          </button>
        </div>
      </label>
      <label class="col" style="gap: 4px">
        <span>{$t("image.fileName")}</span>
        <input class="mono" bind:value={imageName} placeholder="recovered.img" />
      </label>
      <span class="muted">{$t("image.mapfileHint")}</span>

      {#if dest.resumable}
        <div class="notice">{$t("image.willResume", { rescued: bytes(dest.rescued) })}</div>
      {:else if dest.exists}
        <div class="notice warn col" style="gap: 6px">
          <span>{$t("image.willOverwrite")}</span>
          <label class="row">
            <input type="checkbox" bind:checked={imageOverwrite} />
            <span>{$t("image.overwriteConfirm")}</span>
          </label>
        </div>
      {/if}

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
