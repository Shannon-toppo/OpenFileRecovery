//! 走査の共通インターフェース。
//!
//! FAT32 / exFAT のどちらの実装も [`FileSystem`] を実装するので、CLI と GUI は
//! 中身を知らずに扱える。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::error::Result;
use crate::tree::FileTree;

/// 対応しているファイルシステム。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsKind {
    /// FAT32。
    Fat32,
    /// exFAT。
    ExFat,
}

impl FsKind {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            FsKind::Fat32 => "FAT32",
            FsKind::ExFat => "exFAT",
        }
    }
}

impl std::fmt::Display for FsKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// ボリュームの基本情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 種別。
    pub kind: FsKind,
    /// ボリュームラベル。
    pub label: Option<String>,
    /// ボリュームシリアル番号。
    pub serial: Option<u32>,
    /// セクタサイズ。
    pub bytes_per_sector: u32,
    /// クラスタサイズ。
    pub bytes_per_cluster: u32,
    /// クラスタ数。
    pub cluster_count: u32,
    /// ボリューム全体のバイト数。
    pub total_bytes: u64,
    /// データ領域の開始オフセット(解析対象デバイスの先頭から)。
    pub data_offset: u64,
    /// ブートセクタをどこから読めたか。
    pub boot_source: BootSource,
    /// 解析時に気付いたこと(推定で埋めた項目など)。
    pub notes: Vec<String>,
}

/// ブートセクタの入手経路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootSource {
    /// 先頭のブートセクタをそのまま読めた。
    Primary,
    /// 先頭が壊れていたのでバックアップから読んだ。
    Backup,
    /// どちらも壊れていたので、FAT 表の位置などから推定した。
    Estimated,
}

impl BootSource {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            BootSource::Primary => "先頭のブートセクタ",
            BootSource::Backup => "バックアップブートセクタ",
            BootSource::Estimated => "FAT 表からの推定",
        }
    }
}

/// 走査の設定。
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// 削除済み項目を拾うか。
    pub deleted: bool,
    /// ルートから辿れないディレクトリを全クラスタ走査で拾うか。
    ///
    /// クイックフォーマット後の復元はこれが主力になる(PLAN.md 5.3)。
    /// 全域を読むので時間がかかる。
    pub orphans: bool,
    /// 項目数の上限。壊れたボリュームで無限に増えるのを防ぐ。
    pub max_entries: usize,
    /// ディレクトリの深さ上限。
    pub max_depth: u32,
    /// 孤立ディレクトリ走査の読み込み単位。
    pub scan_chunk: u64,
    /// キャンセルフラグ。
    pub cancel: Arc<AtomicBool>,
    /// 進捗イベントの最短間隔(PLAN.md 5.7)。
    pub progress_interval: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            deleted: true,
            orphans: true,
            max_entries: 500_000,
            max_depth: 64,
            scan_chunk: 1 << 20,
            cancel: Arc::new(AtomicBool::new(false)),
            progress_interval: Duration::from_millis(100),
        }
    }
}

impl ScanOptions {
    /// キャンセルされたか。
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// 走査の段階。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    /// ディレクトリツリーを辿っている。
    Directories,
    /// 孤立クラスタを走査している。
    Orphans,
}

impl ScanPhase {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            ScanPhase::Directories => "ディレクトリ走査",
            ScanPhase::Orphans => "孤立クラスタ走査",
        }
    }
}

impl std::fmt::Display for ScanPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 走査の進捗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    /// 段階。
    pub phase: ScanPhase,
    /// 現在位置(バイト)。
    pub position: u64,
    /// 走査対象の全長(バイト)。
    pub total: u64,
    /// ここまでに見つかった項目数。
    pub found: usize,
    /// 経過時間。
    pub elapsed: Duration,
}

/// 進捗コールバック。
pub type ScanProgressFn = Box<dyn FnMut(&ScanProgress) + Send>;

/// 読み取り専用のファイルシステム解析。
pub trait FileSystem {
    /// ボリューム情報。
    fn volume(&self) -> &VolumeInfo;

    /// 走査してツリーを返す。
    fn scan(&self, options: &ScanOptions, progress: Option<ScanProgressFn>) -> Result<FileTree>;
}
