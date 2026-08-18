//! 未対応プラットフォーム用のスタブ。
//!
//! Windows / macOS 以外(開発用の Linux など)でもクレートがビルドでき、
//! [`crate::FileDevice`] によるイメージ解析だけは動くようにするためのもの。

use std::path::Path;

use crate::device::{Device, DeviceInfo};
use crate::error::{DeviceError, Result};

/// このプラットフォームでは列挙できない。
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    Err(unsupported())
}

/// このプラットフォームでは生デバイスを開けない。
pub fn open_device(_id: &str) -> Result<Box<dyn Device>> {
    Err(unsupported())
}

/// このプラットフォームでは判定できない。
pub fn disk_id_for_path(_path: &Path) -> Option<String> {
    None
}

/// このプラットフォームではアンマウントできない。
pub fn unmount_device(_id: &str) -> Result<()> {
    Err(unsupported())
}

fn unsupported() -> DeviceError {
    DeviceError::Unsupported(
        "生デバイスアクセスは Windows と macOS のみ対応 (PLAN.md 1章)".to_string(),
    )
}
