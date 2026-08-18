//! 壊れかけメディアからの吸い出し(イメージング)エンジン。
//!
//! GNU ddrescue と同じ多段パス方式を自前実装したもの(PLAN.md 5.2)。
//! 読める領域を先に確保し、だんだん粒度を細かくして不良域に踏み込む。
//! 中断・再開は ddrescue 互換の mapfile だけで完結する。
//!
//! ```no_run
//! use std::path::Path;
//! use ofr_device::FileDevice;
//! use ofr_image::{ImageOptions, Imager};
//!
//! let device = FileDevice::open("/dev/rdisk4")?;
//! let summary = Imager::new(&device)
//!     .with_options(ImageOptions::default())
//!     .with_progress(|p| println!("{} / {}", p.rescued, p.total))
//!     .run(Path::new("usb.img"), Some(Path::new("usb.img.map")))?;
//!
//! println!("取得率 {:.1}%", summary.rescued_ratio() * 100.0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(unsafe_code)]

pub mod blocks;
mod error;
mod imager;
pub mod mapfile;
mod progress;
mod writer;

pub use blocks::{Block, BlockList, BlockStatus};
pub use error::{ImageError, Result};
pub use imager::{ImageOptions, Imager};
pub use mapfile::{CurrentStatus, MapFile};
pub use progress::{ImageSummary, Pass, Progress, ProgressFn};
pub use writer::ImageWriter;
