//! exFAT の読み取り専用パーサ(PLAN.md 5.3)。
//!
//! FAT32 版と狙いは同じだが、構造が違うぶん有利な点と不利な点がある:
//!
//! - **有利**: ディレクトリエントリは「ファイル + ストリーム + 名前」の組で、
//!   組全体のチェックサムを持つ。削除済みでも組の妥当性を確かめられるので、
//!   全クラスタ走査での誤検出がほとんど出ない。`NoFatChain` が立っていた
//!   ファイルは連続配置が確定しているので、削除後も正確に回収できる
//! - **不利**: FAT32 の `.` / `..` にあたるものがないので、孤立ディレクトリの
//!   親は辿れない。名前も親のエントリ側にあるため、親が見つかるまでは
//!   `dir_00000123` のような仮の名前になる
//!
//! ```no_run
//! use ofr_device::FileDevice;
//! use ofr_exfat::ExfatFs;
//! use ofr_fs::{FileSystem, ScanOptions};
//!
//! let device = FileDevice::open("sd.img")?;
//! let fs = ExfatFs::open(&device)?;
//! let tree = fs.scan(&ScanOptions::default(), None)?;
//! println!("{} 件", tree.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(unsafe_code)]

mod boot;
mod dir;
mod scan;

pub use boot::ExfatBoot;
pub use dir::{
    DirContents, ENTRY_SIZE, ExfatEntry, looks_like_directory, parse_directory, set_checksum,
};
pub use scan::ExfatFs;
