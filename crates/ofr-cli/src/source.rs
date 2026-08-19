//! 復旧元の解決。実体は [`ofr_core::source`] にある。
//!
//! 復旧元を開く処理と安全原則(PLAN.md 6章)のチェックは GUI と共通にしてある。
//! 「起動ディスクを拒否する」「出力先が復旧元と同じデバイスなら拒否する」は
//! 2 か所に分けて片方だけ直す事故が致命的なので、判定は 1 つに寄せる。
//! ここに残っているのは CLI の引数型との橋渡しだけ。

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ofr_device::Device;
use ofr_fs::{FileSystem, FsKind};

pub use ofr_core::source::{
    Volume, check_destination, check_source_selectable, parse_size, same_device,
};

/// `--fs` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FsChoice {
    /// 自動判定。
    Auto,
    /// FAT32 として開く。
    Fat32,
    /// exFAT として開く。
    Exfat,
}

impl FsChoice {
    fn kind(self) -> Option<FsKind> {
        match self {
            FsChoice::Auto => None,
            FsChoice::Fat32 => Some(FsKind::Fat32),
            FsChoice::Exfat => Some(FsKind::ExFat),
        }
    }
}

/// 復旧元を開く。既存のファイルならイメージとして、それ以外はデバイス ID として扱う。
pub fn open_source(source: &str) -> Result<Arc<dyn Device>, Box<dyn Error>> {
    Ok(ofr_core::source::open_source(source)?)
}

/// ファイルシステムのある位置を探す。
pub fn locate(
    device: &dyn Device,
    fs: FsChoice,
    offset: Option<u64>,
) -> Result<Volume, Box<dyn Error>> {
    Ok(ofr_core::source::locate(device, fs.kind(), offset)?)
}

/// 指定された種別でボリュームを開く。
pub fn open_filesystem(
    region: &dyn Device,
    kind: FsKind,
) -> Result<Box<dyn FileSystem + '_>, Box<dyn Error>> {
    Ok(ofr_core::source::open_filesystem(region, kind)?)
}

/// Ctrl-C でキャンセルフラグを立てる。
pub fn install_cancel_handler(cancel: Arc<AtomicBool>, message: &'static str) {
    let result = ctrlc::set_handler(move || {
        eprintln!("\n{message}");
        cancel.store(true, Ordering::Relaxed);
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "Ctrl-C ハンドラを登録できなかった");
    }
}
