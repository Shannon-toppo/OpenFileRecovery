<script lang="ts">
  import { onMount } from "svelte";

  import { locale, locales, t } from "./lib/i18n";
  import { app, applyEvent, startOver } from "./lib/state.svelte";
  import type { Step } from "./lib/state.svelte";
  import { onJobEvent } from "./lib/api";

  import Devices from "./screens/Devices.svelte";
  import Mode from "./screens/Mode.svelte";
  import Setup from "./screens/Setup.svelte";
  import Run from "./screens/Run.svelte";
  import Results from "./screens/Results.svelte";
  import Restore from "./screens/Restore.svelte";

  // 画面の並び。1 本道なので、いまどこにいるかを上に出す。
  const steps: { id: Step; key: string }[] = [
    { id: "devices", key: "steps.device" },
    { id: "mode", key: "steps.mode" },
    { id: "run", key: "steps.run" },
    { id: "results", key: "steps.results" },
    { id: "restore", key: "steps.restore" },
  ];

  // setup は run の準備なので、見出しでは run の位置に置く。
  const positionOf = (step: Step) => steps.findIndex((s) => s.id === (step === "setup" ? "run" : step));

  let current = $derived(positionOf(app.step));

  onMount(() => {
    const unlisten = onJobEvent(applyEvent);
    return () => {
      void unlisten.then((f) => f());
    };
  });
</script>

<div class="shell">
  <header>
    <div class="row spread">
      <div class="row" style="gap: 12px">
        <img src="/icon.png" alt="" width="28" height="28" />
        <div>
          <h1>{$t("app.title")}</h1>
          <div class="muted" style="font-size: 12px">{$t("app.subtitle")}</div>
        </div>
      </div>
      <div class="row">
        {#if app.step !== "devices"}
          <button class="ghost" onclick={startOver}>{$t("common.startOver")}</button>
        {/if}
        <label class="row" style="gap: 6px">
          <span class="muted" style="font-size: 12px">{$t("app.language")}</span>
          <select bind:value={$locale} aria-label={$t("app.language")}>
            {#each locales as l (l.id)}
              <option value={l.id}>{l.label}</option>
            {/each}
          </select>
        </label>
      </div>
    </div>

    <ol class="steps">
      {#each steps as step, i (step.id)}
        <li class:done={i < current} class:now={i === current}>
          <span class="dot">{i + 1}</span>
          <span>{$t(step.key)}</span>
        </li>
      {/each}
    </ol>
  </header>

  <main>
    {#if app.step === "devices"}
      <Devices />
    {:else if app.step === "mode"}
      <Mode />
    {:else if app.step === "setup"}
      <Setup />
    {:else if app.step === "run"}
      <Run />
    {:else if app.step === "results"}
      <Results />
    {:else if app.step === "restore"}
      <Restore />
    {/if}
  </main>

  <footer class="muted">{$t("safety.readOnly")}</footer>
</div>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  header {
    padding: 14px 20px 0;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }

  main {
    flex: 1;
    overflow: auto;
    padding: 20px;
  }

  footer {
    border-top: 1px solid var(--line);
    padding: 6px 20px;
    font-size: 11.5px;
    background: var(--panel);
  }

  .steps {
    display: flex;
    gap: 18px;
    list-style: none;
    margin: 12px 0 0;
    padding: 0;
    font-size: 12.5px;
    color: var(--muted);
    flex-wrap: wrap;
  }

  .steps li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-bottom: 8px;
    border-bottom: 2px solid transparent;
  }

  .steps li.now {
    color: var(--text);
    border-bottom-color: var(--accent);
    font-weight: 600;
  }

  .steps li.done {
    color: var(--text);
  }

  .dot {
    width: 18px;
    height: 18px;
    border-radius: 999px;
    border: 1px solid currentColor;
    display: grid;
    place-items: center;
    font-size: 11px;
  }

  .steps li.now .dot {
    background: var(--accent);
    color: var(--accent-text);
    border-color: transparent;
  }
</style>
