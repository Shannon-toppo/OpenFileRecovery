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

pub use device::{Device, DeviceInfo, DeviceKind};
pub use error::{DeviceError, Result};
pub use file::{DEFAULT_BLOCK_SIZE, FileDevice};
#[cfg(feature = "mock")]
pub use mock::{MockDevice, MockDeviceBuilder, MockStats};
