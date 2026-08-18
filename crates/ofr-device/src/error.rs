//! デバイスアクセスのエラー型。
//!
//! 解析側は「読めなかった領域は記録して先へ進む」ことが前提なので、
//! エラーは種類ごとに区別できる形で返す。呼び出し側が
//! 「リトライする価値があるか」を [`DeviceError::is_retryable`] で判定できる。

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, DeviceError>;

/// デバイス読み込み中に起きうるエラー。
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DeviceError {
    /// OS の読み込みAPIが失敗した。
    #[error("offset {offset} から {len} バイトの読み込みに失敗: {source}")]
    Io {
        /// 失敗した読み込みの開始オフセット。
        offset: u64,
        /// 失敗した読み込みの長さ。
        len: usize,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// メディア側の読み取り不良(不良セクタ等)。リトライで成功しうる。
    #[error("offset {offset} から {len} バイトでメディア読み取りエラー")]
    Media {
        /// 不良が検出された読み込みの開始オフセット。
        offset: u64,
        /// 不良が検出された読み込みの長さ。
        len: usize,
    },

    /// デバイス末尾を越える読み込み要求。呼び出し側のバグを示す。
    #[error("offset {offset} + {len} はデバイスサイズ {device_len} を超えている")]
    OutOfRange {
        /// 要求された開始オフセット。
        offset: u64,
        /// 要求された長さ。
        len: u64,
        /// デバイスの全長。
        device_len: u64,
    },

    /// 必要なバイト数を読み切る前にデバイス末尾に達した。
    #[error("offset {offset}: {needed} バイト必要だが {got} バイトしか読めなかった")]
    UnexpectedEof {
        /// 読み込みの開始オフセット。
        offset: u64,
        /// 要求されたバイト数。
        needed: usize,
        /// 実際に読めたバイト数。
        got: usize,
    },

    /// セクタ境界に整列していない読み込み(Windows の非バッファIO 等)。
    #[error("offset {offset} / len {len} がブロックサイズ {block_size} に整列していない")]
    Unaligned {
        /// 読み込みの開始オフセット。
        offset: u64,
        /// 読み込みの長さ。
        len: usize,
        /// デバイスのブロックサイズ。
        block_size: u32,
    },

    /// 指定されたデバイスが見つからない。
    #[error("デバイスが見つからない: {0}")]
    NotFound(String),

    /// 権限不足。生デバイスアクセスには管理者/root権限が必要。
    #[error("{path} へのアクセス権限がない(管理者権限で実行する必要がある): {source}")]
    PermissionDenied {
        /// 開こうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// このプラットフォーム/バックエンドでは未対応の操作。
    #[error("未対応の操作: {0}")]
    Unsupported(String),
}

impl DeviceError {
    /// 同じ読み込みをやり直す価値があるか。
    ///
    /// メディア不良と一過性の IO エラーは真。範囲外・未整列のような
    /// 呼び出し側のバグや、権限不足は偽。
    pub fn is_retryable(&self) -> bool {
        match self {
            DeviceError::Media { .. } => true,
            DeviceError::Io { source, .. } => !matches!(
                source.kind(),
                io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ),
            _ => false,
        }
    }

    /// メディア側の読み取り不良か(不良セクタとして記録すべきか)。
    pub fn is_media(&self) -> bool {
        matches!(self, DeviceError::Media { .. })
    }

    /// 読み込み位置が判明していればそれを返す。
    pub fn offset(&self) -> Option<u64> {
        match self {
            DeviceError::Io { offset, .. }
            | DeviceError::Media { offset, .. }
            | DeviceError::OutOfRange { offset, .. }
            | DeviceError::UnexpectedEof { offset, .. }
            | DeviceError::Unaligned { offset, .. } => Some(*offset),
            _ => None,
        }
    }
}

/// IO エラーを、種類に応じた [`DeviceError`] に変換する。
pub(crate) fn io_error(offset: u64, len: usize, source: io::Error) -> DeviceError {
    DeviceError::Io {
        offset,
        len,
        source,
    }
}
