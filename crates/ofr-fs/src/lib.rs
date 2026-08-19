//! FAT32 / exFAT 解析の共通土台。
//!
//! [`ofr-fat`](../ofr_fat/index.html) と [`ofr-exfat`](../ofr_exfat/index.html) は
//! 構造こそ違うが、上位(CLI / GUI / コピー / 修復)に見せるものは同じでよい。
//! そこで共通部分をこのクレートに置く。
//!
//! - 解析結果の中間表現 [`RecoveredEntry`] / [`FileTree`](tree::FileTree)(PLAN.md 5.3)
//! - 両 FS で同じ形の 32bit FAT 表 [`Fat32Table`](fat::Fat32Table)
//! - パーティションテーブル解析([`partition`])。物理ディスクのイメージから
//!   ボリュームの位置を割り出す
//! - 復元(デバイス → 出力ファイル)の実処理([`extract`])
//!
//! # 破損耐性
//!
//! ここと、これを使う解析クレートは**不正なバイト列で panic しない**
//! (PLAN.md 6章 5項)。範囲外アクセスになりうる読み出しは [`bytes`] のヘルパを
//! 通し、壊れた項目は「スキップして記録」で処理する。

#![deny(unsafe_code)]

pub mod bytes;
pub mod cache;
mod entry;
mod error;
pub mod extract;
pub mod fat;
pub mod partition;
mod scan;
mod time;
mod tree;

pub use entry::{EntryKind, EntryQuality, EntryStatus, Extent, RecoveredEntry};
pub use error::{FsError, Result};
pub use extract::{ExtractOptions, ExtractStats};
pub use scan::{
    BootSource, FileSystem, FsKind, ScanOptions, ScanPhase, ScanProgress, ScanProgressFn,
    VolumeInfo,
};
pub use time::{Timestamp, Timestamps};
pub use tree::{EntryId, FileTree, ScanStats};
