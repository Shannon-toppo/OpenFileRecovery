//! 修復のエラー型。
//!
//! 「直しきれなかった」はエラーにしない。どこまで直せたかは
//! [`RepairReport`](crate::RepairReport) に載せて `Ok` で返す。ここに出てくるのは
//! 修復を始められない / 結果を書けない種類の失敗だけ。

use std::io;
use std::path::PathBuf;

use ofr_device::DeviceError;
use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, RepairError>;

/// 修復を続行できないエラー。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RepairError {
    /// 入力が見つからない。
    #[error("{0} が見つからない")]
    NotFound(PathBuf),

    /// 入力を開けない / 読めない。
    #[error("修復元 {path} を読めない: {source}")]
    Input {
        /// 読もうとしたパス。
        path: PathBuf,
        /// 元のエラー。
        #[source]
        source: DeviceError,
    },

    /// 出力に書けない。
    #[error("{path} に書き込めない: {source}")]
    Output {
        /// 書き込もうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// 出力先が入力と同じ。
    ///
    /// 修復は必ずコピーに対して行い、元ファイルは残す(PLAN.md 5.6)。
    /// 直しそこねたときに元を失うと取り返しがつかない。
    #[error("出力先が修復元と同じ ({path})。修復結果は別のファイルへ書くこと")]
    SameFile {
        /// 指定されたパス。
        path: PathBuf,
    },

    /// 形式を判定できない。
    #[error("{path} の形式を判定できない。--format で指定するか、対応形式か確認すること")]
    UnknownFormat {
        /// 判定できなかったパス。
        path: PathBuf,
    },

    /// 直すのに情報が足りない。
    ///
    /// 参照ファイルや寸法の指定があれば直せる、という種類の行き止まり。
    /// 「何を渡せば直せるか」を必ず文面に入れること。
    #[error("{0}")]
    NotEnoughInformation(String),

    /// 参照ファイルが使えない。
    #[error("参照ファイル {path} を使えない: {reason}")]
    Reference {
        /// 参照ファイルのパス。
        path: PathBuf,
        /// 使えない理由。
        reason: String,
    },

    /// メモリに載せるには大きすぎる。
    #[error("{size} バイトは修復対象として大きすぎる (上限 {limit} バイト)")]
    TooLarge {
        /// 入力のサイズ。
        size: u64,
        /// 上限。
        limit: u64,
    },
}

impl RepairError {
    /// 出力側の IO エラーを作る。
    pub fn output(path: impl Into<PathBuf>, source: io::Error) -> Self {
        RepairError::Output {
            path: path.into(),
            source,
        }
    }

    /// 参照ファイルのエラーを作る。
    pub fn reference(path: impl Into<PathBuf>, reason: impl Into<String>) -> Self {
        RepairError::Reference {
            path: path.into(),
            reason: reason.into(),
        }
    }
}
