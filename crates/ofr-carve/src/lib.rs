//! シグネチャカービング。ファイルシステムに頼らずファイルを探す。
//!
//! フルフォーマットや FS の全損でディレクトリ情報が失われた場合の最終手段
//! (PLAN.md 5.4)。デバイスやイメージを先頭から走査し、ファイル形式ごとの
//! マジックバイトを見つけ、そこから終端を計算して切り出す。
//!
//! 対応形式は JPEG / PNG / GIF / HEIC / MP4 / MOV / AVI / WAV / MP3 /
//! ZIP(docx, xlsx, pptx を含む)/ PDF。定義は [`signature::SIGNATURES`] の
//! テーブルにまとまっていて、形式の追加は 1 行足すだけで済む。
//!
//! ```no_run
//! use std::path::Path;
//! use ofr_device::FileDevice;
//! use ofr_carve::{CarveOptions, Carver, FileFormat};
//!
//! let device = FileDevice::open("usb.img")?;
//! let report = Carver::new(&device)
//!     .with_options(CarveOptions {
//!         // FAT / exFAT のクラスタサイズが分かっているなら指定すると速く正確になる。
//!         align: 4096,
//!         formats: Some(vec![FileFormat::Jpeg, FileFormat::Mp4]),
//!         ..CarveOptions::default()
//!     })
//!     .with_found(|f| println!("{} at {} ({} バイト)", f.file_name, f.offset, f.size))
//!     .run(Some(Path::new("recovered")))?;
//!
//! println!("{} 件 / 境界確定 {:.0}%", report.summary.found, report.summary.exact_ratio() * 100.0);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # 限界
//!
//! - **元のファイル名は戻らない。** ディレクトリエントリを見ないので原理的に不可能で、
//!   名前は連番と Exif の撮影日時から組み立てる。
//! - **断片化したファイルは繋がらない。** 連続配置を仮定して切り出すので、
//!   途中で別のファイルのクラスタが挟まっていると壊れたデータになる。
//!   その手当ては Phase 5 の修復モジュール(ofr-repair)の仕事。
//! - 終端を確定できなかったものは [`Confidence::Truncated`] が付く。
//!   切り出し長は「次のシグネチャの手前」か形式ごとの最大サイズになる。
//!
//! # 安全原則
//!
//! 読み込みしかしない。復旧元への書き込み経路は [`ofr_device::Device`] に存在せず、
//! 出力は必ず別に指定したディレクトリへ書く(PLAN.md 6章 1項)。
//! 不正なバイト列で panic しないこと(同 5項)はバリデータ側の責務で、
//! ランダムデータに対する走査をテストで確認している。

#![deny(unsafe_code)]

mod carver;
mod error;
mod exif;
mod fill;
mod format;
mod output;
mod progress;
mod reader;
mod result;
mod scanner;
pub mod signature;
mod validate;

pub use carver::{CarveOptions, Carver};
pub use error::{CarveError, Result};
pub use format::{Confidence, FileFormat, FileMetadata, Timestamp};
pub use progress::{CarveProgress, FoundFn, ProgressFn};
pub use result::{CarveReport, CarveSummary, CarvedFile};
pub use signature::{SIGNATURES, Signature};
