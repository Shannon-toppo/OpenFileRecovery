<script lang="ts">
  import { untrack } from "svelte";

  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import type { EntryDto } from "../lib/types";
  import Self from "./TreeNode.svelte";

  export interface Node {
    name: string;
    path: string;
    entry: EntryDto | null;
    children: Node[];
    /** 配下のファイル ID (フォルダの一括選択用)。 */
    fileIds: number[];
  }

  let {
    node,
    depth = 0,
    selected,
    active,
    onToggle,
    onOpen,
  }: {
    node: Node;
    depth?: number;
    selected: Record<number, boolean>;
    active: number | null;
    onToggle: (ids: number[], value: boolean) => void;
    onOpen: (entry: EntryDto) => void;
  } = $props();

  // 深さは節ごとに固定なので、初期値としてだけ読む。
  let expanded = $state(untrack(() => depth) < 2);
  let isDir = $derived(node.entry === null || node.entry.kind === "dir");
  let checked = $derived(node.fileIds.length > 0 && node.fileIds.every((id) => selected[id]));
  let partial = $derived(!checked && node.fileIds.some((id) => selected[id]));

  // 何を疑うべきかを 1 行で伝える (PLAN.md 5.3)。
  // 一覧では短い名前だけを出し、詳しい説明はマウスを乗せたときに見せる。
  function concerns(entry: EntryDto): { label: string; help: string }[] {
    const c = entry.concerns;
    const out: { label: string; help: string }[] = [];
    if (c.contiguousAssumed)
      out.push({
        label: $t("results.concerns.contiguous"),
        help: $t("results.concernHelp.contiguous"),
      });
    if (c.conflictingClusters > 0)
      out.push({
        label: $t("results.concerns.conflicting", { count: c.conflictingClusters }),
        help: $t("results.concernHelp.conflicting"),
      });
    if (c.namePartial)
      out.push({
        label: $t("results.concerns.partialName"),
        help: $t("results.concernHelp.partialName"),
      });
    if (c.truncated)
      out.push({
        label: $t("results.concerns.truncated"),
        help: $t("results.concernHelp.truncated"),
      });
    return out;
  }
</script>

<div class="node" class:active={node.entry !== null && active === node.entry.id}>
  <div class="row" style="padding-left: {depth * 16}px; gap: 6px">
    <input
      type="checkbox"
      checked={checked}
      indeterminate={partial}
      disabled={node.fileIds.length === 0}
      onchange={(e) => onToggle(node.fileIds, e.currentTarget.checked)}
    />

    {#if isDir}
      <button class="twisty" onclick={() => (expanded = !expanded)} aria-expanded={expanded}>
        {expanded ? "▾" : "▸"}
      </button>
    {:else}
      <span class="twisty"></span>
    {/if}

    <button
      class="label grow truncate"
      onclick={() => (node.entry && node.entry.kind === "file" ? onOpen(node.entry) : (expanded = !expanded))}
    >
      {isDir ? "📁" : "📄"}
      {node.name}
    </button>

    {#if node.entry}
      <span class="badge {node.entry.status}">{$t(`results.status.${node.entry.status}`)}</span>
      {#if node.entry.kind === "file"}
        <span class="mono size">{bytes(node.entry.size)}</span>
      {/if}
    {/if}
  </div>

  {#if node.entry && node.entry.kind === "file" && concerns(node.entry).length > 0}
    <div class="concerns muted" style="padding-left: {depth * 16 + 46}px">
      {#each concerns(node.entry) as concern, i (i)}
        {#if i > 0}<span aria-hidden="true"> · </span>{/if}<span title={concern.help}
          >{concern.label}</span
        >
      {/each}
    </div>
  {/if}

  {#if expanded}
    {#each node.children as child (child.path)}
      <Self node={child} depth={depth + 1} {selected} {active} {onToggle} {onOpen} />
    {/each}
  {/if}
</div>

<style>
  .node {
    border-radius: 6px;
  }

  .node.active > .row {
    background: var(--panel-2);
    border-radius: 6px;
  }

  .twisty {
    width: 20px;
    padding: 0;
    border: none;
    background: none;
    color: var(--muted);
    text-align: center;
  }

  .label {
    border: none;
    background: none;
    padding: 2px 0;
    text-align: left;
  }

  .size {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .concerns {
    font-size: 11.5px;
    padding-bottom: 2px;
  }
</style>
