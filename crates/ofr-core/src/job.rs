//! ジョブの依頼・進捗イベント・結果。
//!
//! GUI とコアの間はこの型だけでやり取りする(PLAN.md 4章)。ジョブは
//! 開始すると専用スレッドで走り、進捗・発見・完了をイベントで流す。
//! キャンセルは `AtomicBool` フラグで、途中まで書き出したものは残す。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dto::{
    CarveSummaryDto, CarvedFileDto, CopySummaryDto, FileResultDto, ImageSummaryDto, ProgressDto,
    RepairReportDto, ScanStatsDto, VolumeDto,
};
use crate::error::ErrorCode;

/// ジョブの識別子。解析ジョブの ID は、そのままセッション ID になる。
pub type JobId = u64;

/// ジョブの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    /// 吸い出し(イメージング)。
    Image,
    /// ファイルシステム解析。
    Scan,
    /// 解析結果からの復元。
    Restore,
    /// シグネチャカービング。
    Carve,
    /// 構造保持コピー。
    Copy,
    /// 破損ファイルの修復。
    Repair,
}

/// ファイルシステムの指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsChoice {
    /// 自動判定。
    #[default]
    Auto,
    /// FAT32 として開く。
    Fat32,
    /// exFAT として開く。
    Exfat,
}

impl FsChoice {
    /// 解析クレートに渡す形。`Auto` なら `None`。
    pub fn kind(self) -> Option<ofr_fs::FsKind> {
        match self {
            FsChoice::Auto => None,
            FsChoice::Fat32 => Some(ofr_fs::FsKind::Fat32),
            FsChoice::Exfat => Some(ofr_fs::FsKind::ExFat),
        }
    }
}

/// 宛先に同名のファイルがあったときの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExistingFile {
    /// 番号を足して両方残す。
    #[default]
    Rename,
    /// 飛ばす(中断したコピーの続きに使う)。
    Skip,
    /// 上書きする。
    Overwrite,
}

impl From<ExistingFile> for ofr_copy::ExistingFile {
    fn from(v: ExistingFile) -> Self {
        match v {
            ExistingFile::Rename => ofr_copy::ExistingFile::Rename,
            ExistingFile::Skip => ofr_copy::ExistingFile::Skip,
            ExistingFile::Overwrite => ofr_copy::ExistingFile::Overwrite,
        }
    }
}

fn default_retries() -> u32 {
    3
}
fn default_copy_retries() -> u32 {
    2
}
fn default_chunk() -> u64 {
    1 << 20
}
fn default_align() -> u64 {
    512
}
fn default_max_file_size() -> u64 {
    4 << 30
}
fn yes() -> bool {
    true
}

/// 吸い出し(PLAN.md 5.2)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRequest {
    /// 復旧元のデバイス ID。
    pub source: String,
    /// 出力する raw イメージのパス。
    pub output: PathBuf,
    /// mapfile のパス。省略すると `<出力>.map`。
    #[serde(default)]
    pub mapfile: Option<PathBuf>,
    /// 不良セクタのリトライ回数。
    #[serde(default = "default_retries")]
    pub retries: u32,
    /// コピーパスの読み込み単位。
    #[serde(default = "default_chunk")]
    pub block_size: u64,
    /// トリムパスを行うか。
    #[serde(default = "yes")]
    pub trim: bool,
    /// スクレイプパスを行うか。
    #[serde(default = "yes")]
    pub scrape: bool,
    /// リトライパスを行うか。
    #[serde(default = "yes")]
    pub retry: bool,
    /// 開始前にアンマウントするか(macOS)。
    #[serde(default)]
    pub unmount: bool,
    /// 再開できない既存イメージへの上書きを許すか。
    ///
    /// GUI は既存ファイルを見つけたら確認ダイアログを出し、了承を得てから
    /// これを立てて送り直す。
    #[serde(default)]
    pub overwrite: bool,
}

/// ファイルシステム解析(PLAN.md 5.3)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    /// 復旧元。デバイス ID か、取得済みイメージのパス。
    pub source: String,
    /// ファイルシステムの指定。
    #[serde(default)]
    pub fs: FsChoice,
    /// ボリュームの開始位置。
    #[serde(default)]
    pub offset: Option<u64>,
    /// 削除済みを探すか。
    #[serde(default = "yes")]
    pub deleted: bool,
    /// 孤立クラスタ走査を行うか(フォーマット後の復元はこれが主力)。
    #[serde(default = "yes")]
    pub orphans: bool,
}

/// 解析結果からの復元。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    /// 解析ジョブの ID(= セッション ID)。
    pub session: JobId,
    /// 復元する項目の ID。ディレクトリを指定すると中身も全部入る。
    /// 空なら全ファイル。
    #[serde(default)]
    pub entries: Vec<usize>,
    /// 復元先フォルダ。
    pub dest: PathBuf,
    /// フォルダ構造を作らず平らに並べるか。
    #[serde(default)]
    pub flatten: bool,
    /// 読み込み失敗時のリトライ回数。
    #[serde(default = "default_copy_retries")]
    pub retries: u32,
    /// 読めなかった部分をゼロで埋めるか。
    #[serde(default = "yes")]
    pub zero_fill: bool,
}

/// シグネチャカービング(PLAN.md 5.4)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CarveRequest {
    /// 復旧元。
    pub source: String,
    /// 切り出したファイルを置くディレクトリ。
    pub output: PathBuf,
    /// 探す形式(`jpeg` など)。空なら全形式。
    #[serde(default)]
    pub formats: Vec<String>,
    /// ファイル先頭を探す境界。クラスタサイズが分かるなら指定する。
    #[serde(default = "default_align")]
    pub align: u64,
    /// 1 ファイルの上限。
    #[serde(default = "default_max_file_size")]
    pub max_size: u64,
    /// 走査の開始位置。
    #[serde(default)]
    pub start: Option<u64>,
    /// 走査の終了位置。
    #[serde(default)]
    pub end: Option<u64>,
    /// 終端を確定できなかったファイルも出力するか。
    #[serde(default = "yes")]
    pub include_truncated: bool,
    /// 開始前にアンマウントするか(macOS)。
    #[serde(default)]
    pub unmount: bool,
}

/// 構造保持コピー(PLAN.md 5.5)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRequest {
    /// 復旧元。マウント済みフォルダ、デバイス ID、または取得済みイメージ。
    pub source: String,
    /// 宛先フォルダ。
    pub dest: PathBuf,
    /// ファイルシステムの指定(デバイス / イメージのみ)。
    #[serde(default)]
    pub fs: FsChoice,
    /// ボリュームの開始位置(デバイス / イメージのみ)。
    #[serde(default)]
    pub offset: Option<u64>,
    /// 削除済み・孤立の項目もコピーするか。
    #[serde(default)]
    pub include_deleted: bool,
    /// 宛先に同名があったときの扱い。
    #[serde(default)]
    pub on_existing: ExistingFile,
    /// 読み込み失敗時のリトライ回数。
    #[serde(default = "default_copy_retries")]
    pub retries: u32,
    /// 読み込み単位。
    #[serde(default = "default_chunk")]
    pub chunk_size: u64,
    /// 読めなかった部分をゼロで埋めるか。
    #[serde(default = "yes")]
    pub zero_fill: bool,
    /// 元のタイムスタンプを反映するか。
    #[serde(default = "yes")]
    pub timestamps: bool,
}

/// 破損ファイルの修復(PLAN.md 5.6)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairRequest {
    /// 直したいファイル。中身は書き換えない。
    pub input: PathBuf,
    /// 出力先。修復元と同じパスは指定できない。
    pub output: PathBuf,
    /// 参照ファイル(同じ機器・同じ設定で作られた正常なファイル)。
    #[serde(default)]
    pub reference: Option<PathBuf>,
    /// 形式(`jpeg` / `png` / `avi` / `mp4`)。省略すると中身から判定する。
    #[serde(default)]
    pub format: Option<String>,
    /// ヘッダが失われている場合の幅。
    #[serde(default)]
    pub width: Option<u32>,
    /// ヘッダが失われている場合の高さ。
    #[serde(default)]
    pub height: Option<u32>,
    /// 修復結果を検証するか。
    #[serde(default = "yes")]
    pub verify: bool,
}

/// ジョブの依頼。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobRequest {
    /// 吸い出し。
    Image(ImageRequest),
    /// 解析。
    Scan(ScanRequest),
    /// 復元。
    Restore(RestoreRequest),
    /// カービング。
    Carve(CarveRequest),
    /// コピー。
    Copy(CopyRequest),
    /// 修復。
    Repair(RepairRequest),
}

impl JobRequest {
    /// 種類。
    pub fn kind(&self) -> JobKind {
        match self {
            JobRequest::Image(_) => JobKind::Image,
            JobRequest::Scan(_) => JobKind::Scan,
            JobRequest::Restore(_) => JobKind::Restore,
            JobRequest::Carve(_) => JobKind::Carve,
            JobRequest::Copy(_) => JobKind::Copy,
            JobRequest::Repair(_) => JobKind::Repair,
        }
    }
}

/// 解析ジョブの結果。項目そのものは
/// [`Core::entries`](crate::Core::entries) で取りに行く(数が多いため)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResultDto {
    /// 以後この結果を参照するときの ID。
    pub session: JobId,
    /// ボリューム情報。
    pub volume: VolumeDto,
    /// 統計。
    pub stats: ScanStatsDto,
    /// 見つかった項目の総数。
    pub entry_count: usize,
    /// 解析中の警告(日本語の自由文)。
    pub warnings: Vec<String>,
}

/// カービングジョブの結果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CarveResultDto {
    /// プレビューのときに参照する ID。
    pub session: JobId,
    /// サマリ。
    pub summary: CarveSummaryDto,
}

/// コピー / 復元ジョブの結果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyResultDto {
    /// サマリ。
    pub summary: CopySummaryDto,
    /// 欠けた / 失敗したファイル(先頭 200 件まで)。
    pub incomplete: Vec<FileResultDto>,
}

/// ジョブの結果。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobResult {
    /// 吸い出し。
    Image(ImageSummaryDto),
    /// 解析。
    Scan(Box<ScanResultDto>),
    /// 復元。
    Restore(CopyResultDto),
    /// カービング。
    Carve(Box<CarveResultDto>),
    /// コピー。
    Copy(CopyResultDto),
    /// 修復。
    Repair(Box<RepairReportDto>),
}

/// ジョブの結末。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// やりたいことが全部できた。
    Complete,
    /// 一部しかできなかった。
    Incomplete,
    /// 利用者が中断した。
    Cancelled,
}

/// 進行中に流す注記の重さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoteLevel {
    /// 参考情報。
    Info,
    /// 気を付けるべきこと。
    Warn,
}

/// ジョブが見つけた項目。見つかった順にそのまま流す(間引かない)。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ItemDto {
    /// カービングで切り出したファイル。
    Carved(Box<CarvedFileDto>),
    /// コピー / 復元が終わったファイル。
    File(Box<FileResultDto>),
}

/// ジョブから流れるイベント。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum JobEvent {
    /// 開始した。
    Started {
        /// ジョブ ID。
        job: JobId,
        /// 種類。
        kind: JobKind,
        /// 復旧元の表示名。
        source: String,
        /// 出力先(あれば)。
        dest: Option<String>,
    },
    /// 進捗。100ms 間隔に間引かれている(PLAN.md 5.7)。
    Progress {
        /// ジョブ ID。
        job: JobId,
        /// 中身。
        progress: Box<ProgressDto>,
    },
    /// 項目を 1 つ処理した / 見つけた。
    Item {
        /// ジョブ ID。
        job: JobId,
        /// 中身。
        item: ItemDto,
    },
    /// 注記(日本語の自由文)。
    Note {
        /// ジョブ ID。
        job: JobId,
        /// 重さ。
        level: NoteLevel,
        /// 本文。
        message: String,
    },
    /// 終わった。
    Finished {
        /// ジョブ ID。
        job: JobId,
        /// 結末。
        outcome: Outcome,
        /// 結果。
        result: Box<JobResult>,
    },
    /// 続行できないエラーで終わった。
    Failed {
        /// ジョブ ID。
        job: JobId,
        /// 種別。GUI はこれを見て「管理者で実行し直す」などの案内を出す。
        code: ErrorCode,
        /// 本文(日本語の自由文)。
        message: String,
    },
}

impl JobEvent {
    /// どのジョブのイベントか。
    pub fn job(&self) -> JobId {
        match self {
            JobEvent::Started { job, .. }
            | JobEvent::Progress { job, .. }
            | JobEvent::Item { job, .. }
            | JobEvent::Note { job, .. }
            | JobEvent::Finished { job, .. }
            | JobEvent::Failed { job, .. } => *job,
        }
    }

    /// ジョブの終わりを告げるイベントか。
    pub fn is_terminal(&self) -> bool {
        matches!(self, JobEvent::Finished { .. } | JobEvent::Failed { .. })
    }
}

/// イベントの受け口。GUI は webview への emit を、CLI やテストは
/// 好きな処理をここに渡す。
pub type EventSink = std::sync::Arc<dyn Fn(JobEvent) + Send + Sync + 'static>;
