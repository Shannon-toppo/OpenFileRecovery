<script lang="ts">
  import { revealItemInDir } from "@tauri-apps/plugin-opener";

  import { carved, entries } from "../lib/api";
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app, resetJob } from "../lib/state.svelte";
  import type { ApiError, CarvedFileDto, EntryDto, EntryStatus } from "../lib/types";
  import ErrorBox from "../components/ErrorBox.svelte";
  import Preview from "../components/Preview.svelte";
  import TreeNode from "../components/TreeNode.svelte";
  import type { Node } from "../components/TreeNode.svelte";

  let isCarve = $derived(app.job.result?.kind === "carve" || app.mode === "carve");
  // 解析結果に付いてきたボリューム情報。ジオメトリが推定なら信頼度が落ちるので、
  // それを日本語ベタ書きのメモではなく、翻訳できる文言で出す。
  let volume = $derived(app.job.result?.kind === "scan" ? app.job.result.volume : null);

  let query = $state("");
  let status = $state<EntryStatus | "all">("all");
  let page = $state<{ total: number; files: number; bytes: number; entries: EntryDto[] } | null>(null);
  let carvedFiles = $state<CarvedFileDto[]>([]);
  let error = $state<ApiError | null>(null);
  let loading = $state(false);
  let selected = $state<Record<number, boolean>>({});
  let active = $state<number | null>(null);

  const statuses: (EntryStatus | "all")[] = ["all", "deleted", "orphaned", "intact", "damaged"];

  async function load() {
    if (app.session === null) return;
    loading = true;
    error = null;
    try {
      if (isCarve) {
        carvedFiles = await carved(app.session);
      } else {
        page = await entries(app.session, {
          include: query.trim() ? [query.trim()] : [],
          statuses: status === "all" ? [] : [status],
          limit: 3000,
        });
      }
    } catch (e) {
      error = e as ApiError;
    } finally {
      loading = false;
    }
  }

  // 絞り込みを変えたら引き直す。件数が多いので毎回コアに任せる。
  // 見えていないものが選ばれたままだと「N 件を選択」が実態と合わなくなるので、
  // 絞り込みを変えたら選択も外す。
  $effect(() => {
    void query;
    void status;
    selected = {};
    void load();
  });

  /** ページの項目からツリーを組み立てる。絞り込みで親が落ちても形は保つ。 */
  function buildTree(list: EntryDto[]): Node[] {
    const roots: Node[] = [];
    const byPath = new Map<string, Node>();

    const ensure = (path: string, name: string): Node => {
      const found = byPath.get(path);
      if (found) return found;
      const node: Node = { name, path, entry: null, children: [], fileIds: [] };
      byPath.set(path, node);
      const cut = path.lastIndexOf("/");
      if (cut > 0) {
        // 絞り込みで中間のフォルダが落ちていても、パスから作り直して形を保つ。
        const parentPath = path.slice(0, cut);
        const parent = ensure(parentPath, parentPath.slice(parentPath.lastIndexOf("/") + 1));
        parent.children.push(node);
      } else {
        roots.push(node);
      }
      return node;
    };

    for (const entry of list) {
      const path = entry.path.startsWith("/") ? entry.path : `/${entry.path}`;
      const node = ensure(path, entry.name);
      node.entry = entry;
      if (entry.kind === "file") {
        // 親をすべて辿って、フォルダのチェックで一括選択できるようにする。
        for (let p = path; p.length > 0; p = p.slice(0, p.lastIndexOf("/"))) {
          const parent = byPath.get(p);
          if (parent) parent.fileIds.push(entry.id);
          if (!p.includes("/", 1)) break;
        }
      }
    }
    const sort = (nodes: Node[]) => {
      nodes.sort((a, b) => {
        const ad = a.entry === null || a.entry.kind === "dir";
        const bd = b.entry === null || b.entry.kind === "dir";
        if (ad !== bd) return ad ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      nodes.forEach((n) => sort(n.children));
    };
    sort(roots);
    return roots;
  }

  let tree = $derived(page ? buildTree(page.entries) : []);
  let selectedIds = $derived(
    Object.entries(selected)
      .filter(([, on]) => on)
      .map(([id]) => Number(id)),
  );
  let selectedBytes = $derived(
    (page?.entries ?? [])
      .filter((e) => selected[e.id])
      .reduce((sum, e) => sum + e.recoverable, 0),
  );

  function toggle(ids: number[], value: boolean) {
    for (const id of ids) selected[id] = value;
  }

  function selectAll() {
    for (const entry of page?.entries ?? []) {
      if (entry.kind === "file") selected[entry.id] = true;
    }
  }

  function clearSelection() {
    selected = {};
  }

  function goRestore() {
    app.selection = selectedIds;
    app.selectionBytes = selectedBytes;
    app.step = "restore";
  }

  function carveInstead() {
    app.mode = "carve";
    resetJob();
    app.step = "setup";
  }

  let activeCarved = $derived(carvedFiles.find((f) => f.index === active) ?? null);
  let activeEntry = $derived(page?.entries.find((e) => e.id === active) ?? null);
</script>

<div class="layout">
  <div class="col" style="gap: 12px; min-width: 0">
    <div class="row spread wrap">
      <h2>{isCarve ? $t("results.carved.title") : $t("results.title")}</h2>
      {#if page}
        <span class="muted">
          {$t("results.showing", { shown: page.entries.length, total: page.total })}
        </span>
      {/if}
    </div>

    {#if error}
      <ErrorBox {error} />
    {/if}

    {#if volume}
      <div class="row wrap muted" style="gap: 12px; font-size: 12px">
        <span>{volume.fs}{volume.label ? ` "${volume.label}"` : ""}</span>
        <span>{$t("scan.cluster")} {bytes(volume.clusterSize)}</span>
        <span
          class:warn={volume.bootSource !== "primary"}
          title={$t(`scan.bootSource.${volume.bootSource}`)}
        >
          {$t(`scan.bootSource.${volume.bootSource}`)}
        </span>
      </div>
    {/if}

    {#if isCarve}
      <div class="notice">{$t("results.carved.noNames")}</div>
      <div class="scroll panel" style="flex: 1">
        <div class="cards">
          {#each carvedFiles as file (file.index)}
            <button class="card col" class:active={active === file.index} onclick={() => (active = file.index)}>
              <b class="truncate">{file.name}</b>
              <span class="muted">
                {file.format} · {bytes(file.size)} ·
                {file.confidence === "exact" ? $t("results.carved.exact") : $t("results.carved.truncated")}
              </span>
              {#if file.metadata.timestamp}
                <span class="muted">{$t("results.carved.shotAt")}: {file.metadata.timestamp}</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
      <div class="row">
        {#if carvedFiles[0]?.output}
          <button class="primary" onclick={() => revealItemInDir(carvedFiles[0].output!)}>
            {$t("common.openFolder")}
          </button>
        {/if}
      </div>
    {:else}
      <div class="row wrap" style="gap: 8px">
        <input class="grow" bind:value={query} placeholder={$t("common.search")} />
        {#each statuses as s (s)}
          <button class:primary={status === s} onclick={() => (status = s)}>
            {$t(`results.filter.${s}`)}
          </button>
        {/each}
      </div>

      <div class="scroll panel tree" style="flex: 1">
        {#if loading}
          <p class="muted">{$t("common.loading")}</p>
        {:else if tree.length === 0}
          <div class="col">
            <p>{$t("results.empty")}</p>
            <span class="muted">{$t("results.emptyHint")}</span>
            <div><button onclick={carveInstead}>{$t("results.carveInstead")}</button></div>
          </div>
        {:else}
          {#each tree as node (node.path)}
            <TreeNode
              {node}
              {selected}
              {active}
              onToggle={toggle}
              onOpen={(entry) => (active = entry.id)}
            />
          {/each}
        {/if}
      </div>

      <div class="row spread wrap panel">
        <div class="row wrap">
          <button onclick={selectAll}>{$t("common.selectAll")}</button>
          <button class="ghost" onclick={clearSelection}>{$t("common.clearSelection")}</button>
          <span class="muted">
            {$t("results.selected", {
              count: selectedIds.length,
              size: bytes(selectedBytes),
            })}
          </span>
        </div>
        <button class="primary" disabled={selectedIds.length === 0} onclick={goRestore}>
          {$t("results.restoreSelected")}
        </button>
      </div>
    {/if}
  </div>

  <aside class="panel col">
    <h3>{$t("results.preview")}</h3>
    <Preview
      session={app.session}
      index={active}
      title={activeCarved?.name ?? activeEntry?.name ?? ""}
      subtitle={activeCarved ? `${activeCarved.format} · ${bytes(activeCarved.size)}` : (activeEntry?.path ?? "")}
    />
    {#if activeEntry?.modified}
      <div class="muted" style="font-size: 12px">
        {$t("common.modified")}: {activeEntry.modified}
      </div>
    {/if}
    {#if activeCarved?.metadata.cameraModel}
      <div class="muted" style="font-size: 12px">
        {$t("results.carved.camera")}: {activeCarved.metadata.cameraMake ?? ""}
        {activeCarved.metadata.cameraModel}
      </div>
    {/if}
  </aside>
</div>

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 16px;
    height: 100%;
  }

  @media (max-width: 900px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }

  .tree {
    min-height: 240px;
  }

  aside {
    align-self: start;
    position: sticky;
    top: 0;
  }

  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 8px;
  }

  .card {
    align-items: flex-start;
    text-align: left;
    gap: 2px;
    background: var(--panel-2);
  }

  .card.active {
    border-color: var(--accent);
  }

  .warn {
    color: var(--warn);
  }
</style>
