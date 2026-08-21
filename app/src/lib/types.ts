// ofr-core が返す値の形。Rust 側の crates/ofr-core/src/dto.rs と対になっている。
//
// 状態や段階は「コード」で来る (`deleted`, `copy` など)。表示する文言は
// locales/*.json が持つ。日本語と英語を切り替えるため、コアからは
// 表示用の文字列を受け取らない。

export type ErrorCode =
  | "permissionDenied"
  | "fullDiskAccess"
  | "systemDisk"
  | "sameDevice"
  | "busy"
  | "notFound"
  | "noFilesystem"
  | "badRequest"
  | "io"
  | "other";

export interface ApiError {
  code: ErrorCode;
  message: string;
}

export interface DeviceDto {
  id: string;
  name: string;
  kind: string;
  sizeBytes: number;
  blockSize: number;
  removable: boolean;
  isSystemDisk: boolean;
  selectable: boolean;
  serial: string | null;
}

export interface PrivilegeDto {
  elevated: boolean;
  platform: "macos" | "windows" | "other";
  canRelaunch: boolean;
  neededForRawDevice: boolean;
}

export interface VolumeDto {
  fs: string;
  label: string | null;
  clusterSize: number;
  totalBytes: number;
  offset: number;
  partition: string;
  bootSource: "primary" | "backup" | "estimated";
  notes: string[];
}

export interface ConcernsDto {
  contiguousAssumed: boolean;
  namePartial: boolean;
  conflictingClusters: number;
  truncated: boolean;
}

export type EntryStatus = "intact" | "deleted" | "orphaned" | "damaged";

export interface EntryDto {
  id: number;
  parent: number | null;
  name: string;
  path: string;
  kind: "file" | "dir";
  size: number;
  recoverable: number;
  status: EntryStatus;
  modified: string | null;
  ext: string;
  concerns: ConcernsDto;
}

export interface EntryPage {
  total: number;
  offset: number;
  entries: EntryDto[];
  files: number;
  bytes: number;
}

export interface EntryQuery {
  include?: string[];
  statuses?: EntryStatus[];
  filesOnly?: boolean;
  offset?: number;
  limit?: number;
}

export interface ScanStatsDto {
  dirs: number;
  files: number;
  intact: number;
  deleted: number;
  orphaned: number;
  damaged: number;
  clustersScanned: number;
  truncated: boolean;
  cancelled: boolean;
  elapsedSecs: number;
}

export interface MapSegmentDto {
  pos: number;
  len: number;
  status: "rescued" | "bad" | "nonTried" | "nonTrimmed" | "nonScraped";
}

export interface ProgressDto {
  phase: string;
  pass: number;
  position: number;
  total: number;
  ratio: number;
  itemsDone: number;
  itemsTotal: number;
  bytesDone: number;
  bytesTotal: number;
  rescued: number;
  bad: number;
  pending: number;
  found: number;
  errors: number;
  rate: number;
  etaSecs: number | null;
  elapsedSecs: number;
  current: string;
  map: MapSegmentDto[];
}

export interface ImageSummaryDto {
  total: number;
  rescued: number;
  bad: number;
  remaining: number;
  errors: number;
  reopens: number;
  elapsedSecs: number;
  cancelled: boolean;
  complete: boolean;
  imagePath: string;
  mapPath: string | null;
}

export interface CarvedMetadataDto {
  timestamp: string | null;
  width: number | null;
  height: number | null;
  cameraMake: string | null;
  cameraModel: string | null;
  durationMs: number | null;
}

export interface CarvedFileDto {
  index: number;
  name: string;
  format: string;
  ext: string;
  offset: number;
  size: number;
  confidence: "exact" | "truncated";
  badBytes: number;
  output: string | null;
  metadata: CarvedMetadataDto;
}

export interface FormatCountDto {
  format: string;
  count: number;
  bytes: number;
}

export interface CarveSummaryDto {
  scanned: number;
  found: number;
  exact: number;
  bytesRecovered: number;
  readErrors: number;
  elapsedSecs: number;
  cancelled: boolean;
  byFormat: FormatCountDto[];
  output: string | null;
  reportPath: string | null;
}

export interface FileResultDto {
  source: string;
  output: string;
  size: number;
  written: number;
  missing: number;
  status: "copied" | "partial" | "failed" | "skipped";
  error: string | null;
}

export interface CopySummaryDto {
  files: number;
  copied: number;
  partial: number;
  failed: number;
  skipped: number;
  dirs: number;
  bytesWritten: number;
  bytesMissing: number;
  elapsedSecs: number;
  cancelled: boolean;
  complete: boolean;
  destination: string;
  reportJson: string | null;
  reportText: string | null;
}

export interface RepairReportDto {
  input: string;
  output: string | null;
  reference: string | null;
  format: string;
  status: "intact" | "repaired" | "partial" | "failed";
  inputSize: number;
  outputSize: number;
  fixes: string[];
  issues: string[];
  verification: "decoded" | "container" | "failed" | "skipped";
  verificationDetail: string;
  verified: boolean;
  elapsedSecs: number;
}

export interface OutputState {
  exists: boolean;
  resumable: boolean;
  rescued: number;
  total: number;
}

export interface PreviewDto {
  name: string;
  mime: string;
  data: string;
  bytes: number;
  truncated: boolean;
}

export type JobKind = "image" | "scan" | "restore" | "carve" | "copy" | "repair";

export type JobResult =
  | ({ kind: "image" } & ImageSummaryDto)
  | {
      kind: "scan";
      session: number;
      volume: VolumeDto;
      stats: ScanStatsDto;
      entryCount: number;
      warnings: string[];
    }
  | { kind: "restore"; summary: CopySummaryDto; incomplete: FileResultDto[] }
  | { kind: "carve"; session: number; summary: CarveSummaryDto }
  | { kind: "copy"; summary: CopySummaryDto; incomplete: FileResultDto[] }
  | ({ kind: "repair" } & RepairReportDto);

export type JobEvent =
  | { event: "started"; job: number; kind: JobKind; source: string; dest: string | null }
  | { event: "progress"; job: number; progress: ProgressDto }
  | { event: "item"; job: number; item: ItemDto }
  | { event: "note"; job: number; level: "info" | "warn"; message: string }
  | { event: "finished"; job: number; outcome: Outcome; result: JobResult }
  | { event: "failed"; job: number; code: ErrorCode; message: string };

export type ItemDto =
  | ({ type: "carved" } & CarvedFileDto)
  | ({ type: "file" } & FileResultDto);

export type Outcome = "complete" | "incomplete" | "cancelled";

export type FsChoice = "auto" | "fat32" | "exfat";

export type JobRequest =
  | {
      kind: "image";
      source: string;
      output: string;
      mapfile?: string;
      retries?: number;
      blockSize?: number;
      trim?: boolean;
      scrape?: boolean;
      retry?: boolean;
      unmount?: boolean;
      overwrite?: boolean;
    }
  | {
      kind: "scan";
      source: string;
      fs?: FsChoice;
      offset?: number;
      deleted?: boolean;
      orphans?: boolean;
    }
  | {
      kind: "restore";
      session: number;
      entries?: number[];
      dest: string;
      flatten?: boolean;
      retries?: number;
      zeroFill?: boolean;
    }
  | {
      kind: "carve";
      source: string;
      output: string;
      formats?: string[];
      align?: number;
      maxSize?: number;
      start?: number;
      end?: number;
      includeTruncated?: boolean;
      unmount?: boolean;
    }
  | {
      kind: "copy";
      source: string;
      dest: string;
      fs?: FsChoice;
      offset?: number;
      includeDeleted?: boolean;
      onExisting?: "rename" | "skip" | "overwrite";
      retries?: number;
      chunkSize?: number;
      zeroFill?: boolean;
      timestamps?: boolean;
    }
  | {
      kind: "repair";
      input: string;
      output: string;
      reference?: string;
      format?: string;
      width?: number;
      height?: number;
      verify?: boolean;
    };
