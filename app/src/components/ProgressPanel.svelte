<script lang="ts">
  import { bytes, duration, eta, percent, rate } from "../lib/format";
  import { t } from "../lib/i18n";
  import type { JobKind, ProgressDto } from "../lib/types";
  import RegionMap from "./RegionMap.svelte";

  let {
    progress,
    kind,
    cancelRequested = false,
  }: {
    progress: ProgressDto | null;
    kind: JobKind | null;
    /** 中断を指示済みか。届いていないことを画面で言うために使う。 */
    cancelRequested?: boolean;
  } = $props();

  let imaging = $derived(kind === "image");

  // 進捗イベントは読み込みループの中からしか出ない。壊れかけたメディアは
  // 1 回の read で何十秒も固まることがあり、その間イベントが 1 つも飛ばない。
  // 経過時間まで止まると「アプリが固まった」のか「デバイスが詰まっている」のか
  // 区別がつかないので、時計はこちらで進め、応答待ちであることを明示する。
  let lastEventAt = $state(performance.now());
  let now = $state(performance.now());

  $effect(() => {
    if (progress) lastEventAt = performance.now();
  });

  $effect(() => {
    const timer = setInterval(() => (now = performance.now()), 500);
    return () => clearInterval(timer);
  });

  let sinceEvent = $derived(Math.max(0, (now - lastEventAt) / 1000));
  let elapsed = $derived((progress?.elapsedSecs ?? 0) + sinceEvent);
  /** しばらくイベントが来ていない = デバイスが応答していない。 */
  let stalled = $derived(progress !== null && sinceEvent >= 3);
  /** 中断を指示したのに、デバイスが応答しないので届いていない。 */
  let cancelStuck = $derived(stalled && cancelRequested && sinceEvent >= 8);

  // この画面が出ている間はまだ終わっていないので、100% とは言わない。
  // 残り 0.01% が不良セクタで、そこで何時間も粘ることが実際にある。
  let shown = $derived(Math.min(progress?.ratio ?? 0, 0.999));
</script>

<div class="col" style="gap: 12px">
  <div class="row spread">
    <b>{progress ? $t(`run.phase.${progress.phase}`) : $t("common.loading")}</b>
    <span class="mono">{percent(shown)}</span>
  </div>

  <div class="bar"><div style="width: {shown * 100}%"></div></div>

  {#if progress?.current}
    <div class="muted mono truncate">{progress.current}</div>
  {/if}

  {#if cancelStuck}
    <div class="notice bad col" style="gap: 2px">
      <b>{$t("run.cancelStuck")}</b>
      <span class="muted">{$t("run.cancelStuckHint")}</span>
    </div>
  {:else if stalled}
    <div class="notice warn col" style="gap: 2px">
      <b>{$t("run.stalled", { secs: Math.floor(sinceEvent) })}</b>
      <span class="muted">{$t("run.stalledHint")}</span>
    </div>
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
      <b>{duration(elapsed)}</b><span>{$t("common.elapsed")}</span>
    </span>
    <span class="stat"><b>{progress?.errors ?? 0}</b><span>{$t("common.errors")}</span></span>
  </div>

  {#if imaging && progress && progress.map.length > 0}
    <RegionMap map={progress.map} total={progress.total} />
  {/if}
</div>
