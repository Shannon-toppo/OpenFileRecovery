<script lang="ts">
  import { openPrivacySettings } from "../lib/api";
  import { t } from "../lib/i18n";
  import type { ApiError } from "../lib/types";

  let { error }: { error: ApiError } = $props();

  // コードごとの追加案内。ここで手が止まらないようにする。
  const hints: Partial<Record<ApiError["code"], string>> = {
    noFilesystem: "errors.noFilesystemHint",
    sameDevice: "errors.sameDeviceHint",
    fullDiskAccess: "errors.fullDiskAccessHint",
  };
</script>

<div class="notice bad col">
  <b>{$t(`errors.${error.code}`)}</b>
  <span style="white-space: pre-wrap">{error.message}</span>
  {#if hints[error.code]}
    <span class="muted">{$t(hints[error.code]!)}</span>
  {/if}

  {#if error.code === "fullDiskAccess"}
    <div><button onclick={openPrivacySettings}>{$t("errors.openSettings")}</button></div>
  {/if}
</div>
