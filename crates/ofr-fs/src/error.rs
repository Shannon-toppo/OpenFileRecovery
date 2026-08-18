//! ファイルシステム解析のエラー型。
//!
//! 「その項目が壊れていた」はエラーにしない。スキップして
//! [`FileTree`](crate::FileTree) の警告に記録し、走査は続ける(PLAN.md 5.3)。
//! ここに来るのは、そもそも解析を始められない/続けられない種類の失敗だけ。

use std::io;
use std::path::PathBuf;

use ofr_device::DeviceError;
use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, FsError>;

/// ファイルシステム解析・復元中に起きうるエラー。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FsError {
    /// デバイス側のエラー。
    #[error("デバイスエラー: {0}")]
    Device(#[from] DeviceError),

    /// 出力先の読み書きに失敗した。
    #[error("{path} の書き込みに失敗: {source}")]
    Output {
        /// 対象のパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 対応しているファイルシステムが見つからない。
    #[error("{0} を認識できない(対応しているのは FAT32 と exFAT)")]
    NotRecognized(String),

    /// 認識はできたが、続行できないほど壊れている。
    #[error("{fs} の構造が壊れていて解析を始められない: {detail}")]
    Corrupt {
        /// ファイルシステム名。
        fs: &'static str,
        /// 何が駄目だったか。
        detail: String,
    },

    /// 解析はできるが、この実装では未対応の構成。
    #[error("未対応の構成: {0}")]
    Unsupported(String),

    /// 利用者によるキャンセル。
    #[error("中断された")]
    Cancelled,
}

impl FsError {
    /// 壊れているという意味のエラーを作る。
    pub fn corrupt(fs: &'static str, detail: impl Into<String>) -> Self {
        FsError::Corrupt {
            fs,
            detail: detail.into(),
        }
    }

    /// 出力先の IO エラーを作る。
    pub fn output(path: impl Into<PathBuf>, source: io::Error) -> Self {
        FsError::Output {
            path: path.into(),
            source,
        }
    }
}
