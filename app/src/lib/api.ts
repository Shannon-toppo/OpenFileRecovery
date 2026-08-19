// Tauri コマンドの呼び出し口。ここ以外から invoke を呼ばない。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ApiError,
  CarvedFileDto,
  DeviceDto,
  EntryPage,
  EntryQuery,
  JobEvent,
  JobRequest,
  PreviewDto,
  PrivilegeDto,
} from "./types";

/** ジョブのイベントが流れてくるチャンネル。 */
export const JOB_EVENT = "ofr://job";

/** invoke の失敗を ApiError に揃える。 */
function asApiError(e: unknown): ApiError {
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    return e as ApiError;
  }
  return { code: "other", message: String(e) };
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw asApiError(e);
  }
}

/** 接続されているデバイスの一覧。 */
export const listDevices = () => call<DeviceDto[]>("list_devices");

/** いまの権限。 */
export const privileges = () => call<PrivilegeDto>("privileges");

/** 管理者権限で起動し直す (macOS)。成功するとこのプロセスは終了する。 */
export const relaunchElevated = () => call<void>("relaunch_elevated");

/** ジョブを始める。戻り値はジョブ ID。 */
export const startJob = (request: JobRequest) => call<number>("start_job", { request });

/** ジョブを中断する。 */
export const cancelJob = (job: number) => call<boolean>("cancel_job", { job });

/** 解析結果を 1 ページ取り出す。 */
export const entries = (session: number, query: EntryQuery = {}) =>
  call<EntryPage>("entries", { session, query });

/** カービング結果の一覧。 */
export const carved = (session: number) => call<CarvedFileDto[]>("carved", { session });

/** 中身をプレビュー用に読み出す。 */
export const preview = (session: number, index: number, limit = 0) =>
  call<PreviewDto>("preview", { session, index, limit });

/** 解析結果を捨てる。開いていたデバイスもここで閉じる。 */
export const closeSession = (session: number) => call<void>("close_session", { session });

/** ジョブのイベントを受け取る。 */
export function onJobEvent(handler: (event: JobEvent) => void): Promise<UnlistenFn> {
  return listen<JobEvent>(JOB_EVENT, (e) => handler(e.payload));
}
