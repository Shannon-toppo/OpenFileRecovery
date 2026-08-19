//! ジョブ API のエラー型。
//!
//! GUI はエラーを日本語/英語で出し分けるので、表示用の文字列だけでなく
//! 機械可読な種別([`ErrorCode`])も一緒に返す。GUI はコードを見て
//! 「管理者で実行し直す」ボタンを出すかどうかを決める。

use std::path::PathBuf;

use thiserror::Error;

/// このクレートの共通 Result 型。
pub type Result<T> = std::result::Result<T, CoreError>;

/// GUI が分岐に使うエラー種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// 生デバイスを開く権限がない。管理者 / root で実行し直す必要がある。
    PermissionDenied,
    /// root なのに OS 側の保護で開けない(macOS のフルディスクアクセス)。
    ///
    /// 権限を上げ直しても直らない。[`PermissionDenied`](Self::PermissionDenied) と
    /// 対処が正反対なので、GUI が案内を出し分けられるように分けてある。
    FullDiskAccess,
    /// 起動ディスクを復旧元にしようとした(PLAN.md 6章 3項)。
    SystemDisk,
    /// 出力先が復旧元と同じデバイス上にある(同 2項)。
    SameDevice,
    /// デバイスが使用中で開けない。アンマウントが要る。
    Busy,
    /// 指定されたデバイス / ファイルが見つからない。
    NotFound,
    /// FAT32 / exFAT のボリュームが見つからない。
    NoFilesystem,
    /// 引数の指定が正しくない。
    BadRequest,
    /// 出力の書き込みに失敗した。
    Io,
    /// 上のどれでもないもの。
    Other,
}

/// ジョブ API のエラー。
#[derive(Debug, Error)]
pub enum CoreError {
    /// デバイス層のエラー。
    #[error(transparent)]
    Device(#[from] ofr_device::DeviceError),

    /// ファイルシステム解析のエラー。
    #[error(transparent)]
    Fs(#[from] ofr_fs::FsError),

    /// イメージングのエラー。
    #[error(transparent)]
    Image(#[from] ofr_image::ImageError),

    /// カービングのエラー。
    #[error(transparent)]
    Carve(#[from] ofr_carve::CarveError),

    /// コピーのエラー。
    #[error(transparent)]
    Copy(#[from] ofr_copy::CopyError),

    /// 修復のエラー。
    #[error(transparent)]
    Repair(#[from] ofr_repair::RepairError),

    /// 出力の書き込みなど、こちら側の IO エラー。
    #[error("{path} の書き込みに失敗: {source}")]
    Io {
        /// 対象のパス。
        path: PathBuf,
        /// 元の IO エラー。
        #[source]
        source: std::io::Error,
    },

    /// 起動ディスクを復旧元にしようとした(PLAN.md 6章 3項)。
    #[error("{0} は起動ディスクなので復旧元にできない")]
    SystemDisk(String),

    /// 出力先が復旧元と同じデバイス上にある(PLAN.md 6章 2項)。
    #[error("出力先 {dest} は復旧元 {device} と同じデバイス上にある。別のディスクを指定すること")]
    SameDevice {
        /// 復旧元のデバイス ID。
        device: String,
        /// 出力先のパス。
        dest: String,
    },

    /// FAT32 / exFAT のボリュームが見つからない。
    #[error(
        "FAT32 / exFAT のボリュームが見つからない(候補 {candidates} 件)。\
         位置が分かっているならオフセットを直接指定する"
    )]
    NoFilesystem {
        /// 調べたパーティション候補の数。
        candidates: usize,
    },

    /// 引数の指定が正しくない。
    #[error("{0}")]
    BadRequest(String),

    /// 参照されたセッションがない(GUI の状態がずれている)。
    #[error("解析結果が見つからない (セッション {0})。もう一度スキャンすること")]
    NoSession(u64),
}

impl CoreError {
    /// GUI が分岐に使う種別。
    pub fn code(&self) -> ErrorCode {
        match self {
            CoreError::Device(e) => match e {
                ofr_device::DeviceError::PermissionDenied { .. } => ErrorCode::PermissionDenied,
                ofr_device::DeviceError::OsProtected { .. } => ErrorCode::FullDiskAccess,
                ofr_device::DeviceError::Busy { .. } => ErrorCode::Busy,
                ofr_device::DeviceError::NotFound(_) => ErrorCode::NotFound,
                _ => ErrorCode::Other,
            },
            CoreError::Io { .. } => ErrorCode::Io,
            CoreError::SystemDisk(_) => ErrorCode::SystemDisk,
            CoreError::SameDevice { .. } => ErrorCode::SameDevice,
            CoreError::NoFilesystem { .. } => ErrorCode::NoFilesystem,
            CoreError::BadRequest(_) | CoreError::NoSession(_) => ErrorCode::BadRequest,
            _ => ErrorCode::Other,
        }
    }

    /// エラー本体と、その原因を辿った 1 行の説明。
    pub fn full_message(&self) -> String {
        let mut text = self.to_string();
        let mut source = std::error::Error::source(self);
        while let Some(s) = source {
            let line = s.to_string();
            // thiserror の transparent 変換で同じ文言が二重に出るのを避ける。
            if !text.contains(&line) {
                text.push_str(": ");
                text.push_str(&line);
            }
            source = std::error::Error::source(s);
        }
        text
    }
}
