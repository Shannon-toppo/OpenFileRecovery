//! FAT32 の読み取り専用パーサ(PLAN.md 5.3)。
//!
//! 通常のディレクトリツリーに加えて、次の 2 つを拾うのが目的:
//!
//! - **削除ファイル**: 先頭バイトが `0xE5` になったエントリ。LFN が残っていれば
//!   元の名前もそのまま復元できる。本体は FAT チェーンが解放されているので、
//!   開始クラスタから連続配置と仮定して回収する
//! - **孤立ディレクトリ**: ルートから辿れないディレクトリクラスタ。クイック
//!   フォーマットは FAT 表とルートしか消さないので、サブディレクトリのクラスタは
//!   大半が残っている。フォーマット後の復元はこれが主力になる
//!
//! ```no_run
//! use ofr_device::FileDevice;
//! use ofr_fat::Fat32Fs;
//! use ofr_fs::{FileSystem, ScanOptions};
//!
//! let device = FileDevice::open("usb.img")?;
//! let fs = Fat32Fs::open(&device)?;
//! let tree = fs.scan(&ScanOptions::default(), None)?;
//! for entry in tree.entries() {
//!     println!("{} {}", entry.status, entry.path);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # 破損耐性
//!
//! パース中のエラーは全て「その項目をスキップして続行」で処理し、panic しない
//! (PLAN.md 6章 5項)。ブートセクタが壊れていればバックアップ(セクタ 6)を使い、
//! それも駄目なら FAT 表の位置からジオメトリを推定する。

#![deny(unsafe_code)]

mod bpb;
mod dir;
mod scan;

pub use bpb::Fat32Bpb;
pub use dir::{
    ATTR_DIRECTORY, ATTR_HIDDEN, ATTR_LFN, ATTR_READ_ONLY, ATTR_SYSTEM, ATTR_VOLUME_ID,
    DirContents, DirEntry, ENTRY_SIZE, looks_like_directory, parse_directory, short_name_checksum,
};
pub use scan::Fat32Fs;
