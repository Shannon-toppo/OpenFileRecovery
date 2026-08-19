<script lang="ts">
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import type { MapSegmentDto } from "../lib/types";

  let { map, total }: { map: MapSegmentDto[]; total: number } = $props();

  // 帯グラフ (PLAN.md 5.2)。どこが読めていないかを一目で見せる。
  const colors: Record<MapSegmentDto["status"], string> = {
    rescued: "var(--ok)",
    bad: "var(--bad)",
    nonTried: "var(--panel-2)",
    nonTrimmed: "var(--warn)",
    nonScraped: "var(--warn)",
  };

  const kinds: MapSegmentDto["status"][] = ["rescued", "bad", "nonTrimmed", "nonTried"];

  let width = (segment: MapSegmentDto) => (total > 0 ? (segment.len / total) * 100 : 0);
</script>

<div class="col" style="gap: 6px">
  <div class="row spread">
    <span class="muted" style="font-size: 12px">{$t("run.regionMap")}</span>
    <span class="muted mono" style="font-size: 11.5px">{bytes(total)}</span>
  </div>

  <div class="map">
    {#each map as segment, i (i)}
      <div
        style="width: {width(segment)}%; background: {colors[segment.status]}"
        title="{$t(`run.map.${segment.status}`)} · {bytes(segment.pos)} + {bytes(segment.len)}"
      ></div>
    {/each}
  </div>

  <div class="row wrap" style="gap: 12px">
    {#each kinds as kind (kind)}
      <span class="legend">
        <i style="background: {colors[kind]}"></i>
        {$t(`run.map.${kind}`)}
      </span>
    {/each}
  </div>
</div>

<style>
  .map {
    display: flex;
    height: 18px;
    border: 1px solid var(--line);
    border-radius: 4px;
    overflow: hidden;
    background: var(--panel-2);
  }

  .map > div {
    height: 100%;
  }

  .legend {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11.5px;
    color: var(--muted);
  }

  .legend i {
    width: 10px;
    height: 10px;
    border-radius: 2px;
    border: 1px solid var(--line);
    display: inline-block;
  }
</style>
