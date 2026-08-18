//! Open File Recovery の生デバイスアクセス層。
//!
//! 全クレートの土台。実デバイス・ディスクイメージ・テスト用モックを
//! [`Device`] という一つの読み取り専用 trait で扱う。
//!
//! ```
//! use ofr_device::{Device, MockDevice};
//!
//! let dev = MockDevice::builder(4096).pattern().bad_range(1024, 512).build();
//! let mut buf = [0u8; 512];
//!
//! // 健全な領域は読める。
//! assert_eq!(dev.read_at(0, &mut buf).unwrap(), 512);
//! // 不良領域はメディアエラーになる。
//! assert!(dev.read_at(1024, &mut buf).unwrap_err().is_media());
//! ```
//!
//! # 安全原則
//!
//! このクレートは**読み込みしか行わない**。復旧元デバイスへの書き込み経路を
//! コンパイル時点で存在させないため、[`Device`] に書き込みメソッドを足さないこと
//! (PLAN.md 6章 1項)。

// unsafe は OS の FFI (Phase 1 の Windows/macOS 実装) でのみ、モジュール単位の
// #[allow(unsafe_code)] で明示的に解禁する。
#![deny(unsafe_code)]

pub mod align;
mod device;
mod error;
mod file;
#[cfg(feature = "mock")]
mod mock;
mod sys;

pub use device::{Device, DeviceInfo, DeviceKind};
pub use error::{DeviceError, Result};
pub use file::{DEFAULT_BLOCK_SIZE, FileDevice};
#[cfg(feature = "mock")]
pub use mock::{MockDevice, MockDeviceBuilder, MockStats};

#[cfg(target_os = "macos")]
pub use sys::macos::MacDevice;
#[cfg(windows)]
pub use sys::windows::WindowsDevice;

/// 接続されている物理デバイスを列挙する。
///
/// 起動ディスクも一覧には出るが [`DeviceInfo::is_system_disk`] が立っていて
/// [`DeviceInfo::is_selectable_as_source`] が偽になる(PLAN.md 6章 3項)。
/// 列挙自体は管理者権限なしでも動く(サイズ等が取れない項目は 0 になる)。
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    sys::platform::list_devices()
}

/// デバイスを読み取り専用で開く。
///
/// `id` は [`list_devices`] が返す [`DeviceInfo::id`](Windows なら
/// `\\.\PhysicalDrive2`、macOS なら `/dev/disk4`)。生デバイスの読み込みには
/// 管理者 / root 権限が必要。
pub fn open_device(id: &str) -> Result<Box<dyn Device>> {
    sys::platform::open_device(id)
}

/// 与えたパスが載っている物理デバイスの ID。判定できなければ `None`。
///
/// 「復元先が復旧元と同じデバイス」を弾くために使う(PLAN.md 6章 2項)。
pub fn disk_id_for_path(path: &std::path::Path) -> Option<String> {
    sys::platform::disk_id_for_path(path)
}

/// デバイスをアンマウントする。
///
/// macOS は `diskutil unmountDisk`。Windows は不要なので
/// [`DeviceError::Unsupported`] を返す。**必ずユーザーの確認を取ってから呼ぶこと**。
pub fn unmount_device(id: &str) -> Result<()> {
    sys::platform::unmount_device(id)
}
