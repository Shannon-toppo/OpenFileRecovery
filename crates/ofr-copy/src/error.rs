//! コピーのエラー型。
//!
//! 「このファイルが読めなかった」はエラーにしない。読めた分を書き出し、
//! レポートに記録して次のファイルへ進む(PLAN.md 5.5)。ここに出てくるのは
//! ジョブ全体を止める種類の失敗だけ。

use std::io;
use std::path::PathBuf;

use ofr_device::DeviceError;
use ofr_fs::FsError;
use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, CopyError>;

/// コピーを続行できないエラー。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CopyError {
    /// 復旧元を読めない(ディレクトリを開けない等)。
    #[error("復旧元 {path} を読めない: {source}")]
    Source {
        /// 読もうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 宛先に書けない。
    #[error("{path} に書き込めない: {source}")]
    Output {
        /// 書き込もうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 宛先が復旧元の中にある。
    ///
    /// そのままコピーすると自分が書いたファイルを読み直して無限に増えるので、
    /// 始める前に止める。
    #[error("宛先 {dest} は復旧元 {root} の中にある。別の場所を指定すること")]
    DestinationInsideSource {
        /// 復旧元のルート。
        root: PathBuf,
        /// 指定された宛先。
        dest: PathBuf,
    },

    /// ファイルシステム解析側のエラー。
    #[error(transparent)]
    Fs(#[from] FsError),

    /// デバイス側のエラー。
    #[error("デバイスエラー: {0}")]
    Device(#[from] DeviceError),
}

impl CopyError {
    /// 復旧元の IO エラーを作る。
    pub fn source_io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        CopyError::Source {
            path: path.into(),
            source,
        }
    }

    /// 宛先の IO エラーを作る。
    pub fn output(path: impl Into<PathBuf>, source: io::Error) -> Self {
        CopyError::Output {
            path: path.into(),
            source,
        }
    }
}
