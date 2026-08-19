<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";

  import { listDevices, privileges, relaunchElevated } from "../lib/api";
  import { bytes } from "../lib/format";
  import { t } from "../lib/i18n";
  import { app } from "../lib/state.svelte";
  import type { ApiError, DeviceDto, PrivilegeDto } from "../lib/types";
  import ErrorBox from "../components/ErrorBox.svelte";

  let devices = $state<DeviceDto[]>([]);
  let privilege = $state<PrivilegeDto | null>(null);
  let loading = $state(true);
  let error = $state<ApiError | null>(null);

  async function refresh() {
    loading = true;
    error = null;
    try {
      devices = await listDevices();
      privilege = await privileges();
    } catch (e) {
      error = e as ApiError;
    } finally {
      loading = false;
    }
  }

  function choose(device: DeviceDto) {
    if (!device.selectable) return;
    app.device = device;
    app.source = {
      id: device.id,
      label: device.name || device.id,
      size: device.sizeBytes,
      isImage: false,
    };
    app.step = "mode";
  }

  async function openImage() {
    const path = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Disk image", extensions: ["img", "dd", "raw", "bin", "iso", "dmg"] }],
    });
    if (typeof path !== "string") return;
    app.device = null;
    app.source = {
      id: path,
      label: path.split(/[\\/]/).pop() ?? path,
      size: 0,
      isImage: true,
    };
    app.step = "mode";
  }

  async function elevate() {
    try {
      await relaunchElevated();
    } catch (e) {
      error = e as ApiError;
    }
  }

  onMount(refresh);
</script>

<div class="col" style="gap: 16px; max-width: 940px">
  <div>
    <h2>{$t("devices.title")}</h2>
    <p class="muted" style="margin: 4px 0 0">{$t("devices.lead")}</p>
  </div>

  {#if error}
    <ErrorBox {error} />
  {/if}

  {#if privilege && !privilege.elevated}
    <div class="notice warn col">
      <b>{$t("devices.adminNeeded")}</b>
      <span class="muted">
        {privilege.platform === "windows"
          ? $t("devices.adminExplainWin")
          : $t("devices.adminExplainMac")}
      </span>
      {#if privilege.canRelaunch}
        <div><button onclick={elevate}>{$t("devices.relaunch")}</button></div>
      {/if}
    </div>
  {:else if privilege}
    <div class="muted" style="font-size: 12px">✓ {$t("devices.adminOk")}</div>
  {/if}

  <div class="panel col">
    <div class="row spread">
      <h3>{$t("steps.device")}</h3>
      <button class="ghost" onclick={refresh} disabled={loading}>{$t("common.refresh")}</button>
    </div>

    {#if loading}
      <p class="muted">{$t("common.loading")}</p>
    {:else if devices.length === 0}
      <p class="muted">{$t("devices.empty")}</p>
    {:else}
      <ul class="devices">
        {#each devices as device (device.id)}
          <li>
            <button
              class="device"
              class:disabled={!device.selectable}
              disabled={!device.selectable}
              onclick={() => choose(device)}
            >
              <div class="grow col" style="gap: 2px; align-items: flex-start">
                <div class="row" style="gap: 8px">
                  <b>{device.name || device.id}</b>
                  <span class="badge">{device.removable ? $t("devices.removable") : $t("devices.internal")}</span>
                  {#if device.isSystemDisk}
                    <span class="badge damaged">{$t("devices.systemDisk")}</span>
                  {:else if !device.selectable}
                    <span class="badge">{$t("devices.notSelectable")}</span>
                  {/if}
                </div>
                <div class="muted mono">
                  {device.id}
                  {#if device.serial}&nbsp;· {$t("devices.serial")} {device.serial}{/if}
                </div>
              </div>
              <div class="size mono">{bytes(device.sizeBytes)}</div>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="panel col">
    <div class="row spread">
      <h3>{$t("devices.openImage")}</h3>
      <button onclick={openImage}>{$t("common.choose")}</button>
    </div>
    <span class="muted">{$t("devices.imageHint")}</span>
  </div>
</div>

<style>
  .devices {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .device {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    text-align: left;
    background: var(--panel-2);
    padding: 10px 14px;
  }

  .device.disabled {
    opacity: 0.55;
  }

  .size {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
</style>
