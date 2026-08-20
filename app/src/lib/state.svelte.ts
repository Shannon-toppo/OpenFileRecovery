// 画面をまたいで持ち回す状態。
//
// 画面フローは 1 本道にする (PLAN.md 7章)。トラブル中の利用者に選択肢を
// 並べるより、順番に進めるほうが迷わない。

import * as api from "./api";
import type {
  ApiError,
  DeviceDto,
  JobEvent,
  JobKind,
  JobRequest,
  JobResult,
  ProgressDto,
} from "./types";

/** やること。PLAN.md 7章 2 の 4 つ + 最終手段のカービング。 */
export type Mode = "deleted" | "formatted" | "image" | "copy" | "carve";

/** 画面。 */
export type Step = "devices" | "mode" | "setup" | "run" | "results" | "restore";

/** 復旧元。デバイスかイメージファイル。 */
export interface Source {
  /** ofr-core に渡す文字列 (デバイス ID かパス)。 */
  id: string;
  /** 画面に出す名前。 */
  label: string;
  /** 容量。分からなければ 0。 */
  size: number;
  /** イメージファイルか。 */
  isImage: boolean;
}

/** 進行中 / 直近のジョブ。 */
export interface JobState {
  id: number | null;
  kind: JobKind | null;
  running: boolean;
  progress: ProgressDto | null;
  notes: { level: "info" | "warn"; message: string }[];
  result: JobResult | null;
  outcome: "complete" | "incomplete" | "cancelled" | null;
  error: ApiError | null;
  /** コピー / 復元で終わったファイル (末尾 200 件まで)。 */
  files: { path: string; status: string }[];
  /** 切り出したファイルの数。 */
  carved: number;
  /** 中断を指示したか。応答しないデバイスでは指示が届かないことがある。 */
  cancelRequested: boolean;
}

function emptyJob(): JobState {
  return {
    id: null,
    kind: null,
    running: false,
    progress: null,
    notes: [],
    result: null,
    outcome: null,
    error: null,
    files: [],
    carved: 0,
    cancelRequested: false,
  };
}

/** アプリ全体の状態。 */
export const app = $state({
  step: "devices" as Step,
  source: null as Source | null,
  device: null as DeviceDto | null,
  mode: null as Mode | null,
  /** 解析 / カービング結果のセッション ID。 */
  session: null as number | null,
  /** 結果ツリーで選ばれた項目の ID。 */
  selection: [] as number[],
  /** 選ばれた項目の合計バイト数(表示用)。 */
  selectionBytes: 0,
  job: emptyJob(),
});

/** ジョブの状態を空に戻す。 */
export function resetJob() {
  app.job = emptyJob();
}

/** 最初の画面に戻す。 */
export function startOver() {
  void dropSession();
  app.step = "devices";
  app.source = null;
  app.device = null;
  app.mode = null;
  resetJob();
}

/** ジョブのイベントを状態に取り込む。 */
export function applyEvent(event: JobEvent) {
  const job = app.job;
  if (job.id !== null && event.job !== job.id) return;

  switch (event.event) {
    case "started":
      job.kind = event.kind;
      job.running = true;
      break;
    case "progress":
      job.progress = event.progress;
      break;
    case "note":
      job.notes.push({ level: event.level, message: event.message });
      break;
    case "item":
      if (event.item.type === "carved") {
        job.carved += 1;
      } else {
        // 全部持つと数十万件になりうるので、直近だけ残す。
        job.files.push({ path: event.item.source, status: event.item.status });
        if (job.files.length > 200) job.files.shift();
      }
      break;
    case "finished":
      job.running = false;
      job.outcome = event.outcome;
      job.result = event.result;
      if (event.result.kind === "scan" || event.result.kind === "carve") {
        app.session = event.result.session;
      }
      break;
    case "failed":
      job.running = false;
      job.error = { code: event.code, message: event.message };
      break;
  }
}

/** ジョブを始める。前の結果は捨てる。 */
export async function runJob(request: JobRequest) {
  // 新しく解析し直すなら、前の結果は用済み。デバイスを掴んだままにしない。
  if (request.kind === "scan" || request.kind === "carve") {
    await dropSession();
  }
  resetJob();
  app.job.running = true;
  try {
    app.job.id = await api.startJob(request);
  } catch (e) {
    app.job.running = false;
    app.job.error = e as ApiError;
  }
}

/** 解析結果を捨てる。開いていたデバイスもここで閉じる。 */
export async function dropSession() {
  if (app.session === null) return;
  const session = app.session;
  app.session = null;
  app.selection = [];
  app.selectionBytes = 0;
  try {
    await api.closeSession(session);
  } catch {
    // 閉じられなくても復旧作業は続けられる。
  }
}

/** 走っているジョブを止める。書き出し済みのものは残る。 */
export async function stopJob() {
  if (app.job.id === null) return;
  // 応答しないデバイスでは、読み込みから戻るまでこの指示は届かない。
  // 指示を出したことを覚えておいて、届いていないなら画面でそう言う。
  app.job.cancelRequested = true;
  await api.cancelJob(app.job.id);
}
