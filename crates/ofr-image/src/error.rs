//! イメージングのエラー型。

use std::io;
use std::path::PathBuf;

use ofr_device::DeviceError;
use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, ImageError>;

/// イメージング中に起きうるエラー。
///
/// 読み取り不良そのものはエラーにしない。不良領域はマップに記録して先へ進む
/// (PLAN.md 5.2「1セクタに固執して全体を止めないことが最重要」)。ここに来るのは
/// 続行できない種類の失敗だけ。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImageError {
    /// デバイス側の続行不能なエラー(権限不足など)。
    #[error("デバイスエラー: {0}")]
    Device(#[from] DeviceError),

    /// 出力先ファイルの読み書きに失敗した。
    #[error("{path} の読み書きに失敗: {source}")]
    Io {
        /// 対象のパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// mapfile の書式が壊れている。
    #[error("mapfile {line} 行目: {message}")]
    MapFormat {
        /// 行番号(1 始まり。0 はファイル全体)。
        line: usize,
        /// 内容。
        message: String,
    },

    /// 再開しようとした mapfile が対象デバイスと食い違う。
    #[error("mapfile のサイズ {map_total} がデバイスサイズ {device_len} と一致しない")]
    MapMismatch {
        /// mapfile が示す全長。
        map_total: u64,
        /// 実際のデバイスの全長。
        device_len: u64,
    },

    /// 設定値が不正。
    #[error("設定が不正: {0}")]
    InvalidOptions(String),
}
