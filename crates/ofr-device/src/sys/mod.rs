//! OS 別の生デバイスアクセスと列挙。
//!
//! 上位クレートはここを直接使わず、クレートルートの [`crate::list_devices`] /
//! [`crate::open_device`] 経由で使う。対応 OS は Windows と macOS(PLAN.md 1章)。
//! それ以外の unix ではイメージファイル([`crate::FileDevice`])だけが動く。

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(windows)]
pub mod windows;

#[cfg(not(any(target_os = "macos", windows)))]
pub mod unsupported;

#[cfg(target_os = "macos")]
pub use macos as platform;
#[cfg(not(any(target_os = "macos", windows)))]
pub use unsupported as platform;
#[cfg(windows)]
pub use windows as platform;
