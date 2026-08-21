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
    #[error("offset {offset} + {len}はデバイスサイズ{device_len}を超えています")]
    OutOfRange {
        /// 要求された開始オフセット。
        offset: u64,
        /// 要求された長さ。
        len: u64,
        /// デバイスの全長。
        device_len: u64,
    },

    /// 必要なバイト数を読み切る前にデバイス末尾に達した。
    #[error("offset {offset}: {needed}バイト必要だが{got}バイトしか読めませんでした")]
    UnexpectedEof {
        /// 読み込みの開始オフセット。
        offset: u64,
        /// 要求されたバイト数。
        needed: usize,
        /// 実際に読めたバイト数。
        got: usize,
    },

    /// セクタ境界に整列していない読み込み(Windows の非バッファIO 等)。
    #[error("offset {offset} / len {len}がブロックサイズ{block_size}に整列していません")]
    Unaligned {
        /// 読み込みの開始オフセット。
        offset: u64,
        /// 読み込みの長さ。
        len: usize,
        /// デバイスのブロックサイズ。
        block_size: u32,
    },

    /// 指定されたデバイスが見つからない。
    #[error("デバイスが見つかりません: {0}")]
    NotFound(String),

    /// 権限不足。生デバイスアクセスには管理者/root権限が必要。
    #[error("{path} へのアクセス権限がありません(管理者権限で実行する必要があります): {source}")]
    PermissionDenied {
        /// 開こうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// root / 管理者で動いているのに、OS 側の保護で開けない。
    ///
    /// macOS の TCC がこれ。デバイスノードの POSIX 権限上は root が読めるのに
    /// `EPERM` が返る。生ディスクへのアクセスにはアプリ自身に
    /// **フルディスクアクセス**が要るためで、権限を上げるだけでは解決しない
    /// (PLAN.md 10章)。ターミナルから `sudo` で動かすと通るのは、
    /// ターミナルに与えられたフルディスクアクセスを引き継ぐから。
    #[error(
        "{path} は OS の保護で開けない(root だが拒否された)。\
         macOS の場合は システム設定 > プライバシーとセキュリティ > フルディスクアクセス で\
         このアプリを許可するか、フルディスクアクセスを持つターミナルから sudo で実行すること"
    )]
    OsProtected {
        /// 開こうとしたパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: io::Error,
    },

    /// デバイスが他プロセス/OS に掴まれていて開けない。
    #[error(
        "{path} は使用中で開けません(macOSでは`diskutil unmountDisk`でアンマウントしてください)"
    )]
    Busy {
        /// 開こうとしたパス。
        path: PathBuf,
    },

    /// デバイス情報の取得に失敗した(ioctl / 列挙 API の失敗)。
    #[error("デバイス情報の取得に失敗: {what}: {source}")]
    Query {
        /// 何を取得しようとしたか。
        what: String,
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
            DeviceError::Busy { .. } => true,
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

/// 生デバイスの読み込みエラーを分類する。
///
/// メディア不良(`EIO` 相当)は [`DeviceError::Media`] にして、上位の
/// イメージングエンジンが「不良領域として記録して先へ進む」判断をできるようにする。
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn classify_read_error(offset: u64, len: usize, source: io::Error) -> DeviceError {
    #[cfg(target_os = "macos")]
    let media = matches!(source.raw_os_error(), Some(libc::EIO) | Some(libc::ENXIO));
    // 媒体側の読み取り不良を示す Win32 エラー: ERROR_NOT_READY(21), ERROR_CRC(23),
    // ERROR_SECTOR_NOT_FOUND(27), ERROR_READ_FAULT(30), ERROR_IO_DEVICE(1117),
    // ERROR_DISK_OPERATION_FAILED(1127)。
    #[cfg(windows)]
    let media = matches!(
        source.raw_os_error(),
        Some(21) | Some(23) | Some(27) | Some(30) | Some(1117) | Some(1127)
    );

    if media {
        DeviceError::Media { offset, len }
    } else {
        DeviceError::Io {
            offset,
            len,
            source,
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
