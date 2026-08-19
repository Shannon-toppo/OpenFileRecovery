//! 構造保持コピー。デバイスのデータとフォルダ構造をそのまま宛先へ写す。
//!
//! 要件「デバイスのデータとフォルダ構造をそのままコピー」に対応する部分
//! (PLAN.md 5.5)。復元(`ofr restore`)が「消えたものを選んで拾う」のに対して、
//! こちらは「いま入っているものを丸ごと、同じ形で別のディスクへ移す」。
//!
//! 読み出し経路は 2 つあり、どちらでも宛先にできるミラーツリーは同じ:
//!
//! | 経路 | ソース | 使いどころ |
//! |---|---|---|
//! | 論理コピー | [`MountSource`] | OS がマウントできているデバイス |
//! | 直読み / イメージ展開 | [`TreeSource`] | マウントできないデバイス、`ofr image` で取ったイメージ |
//!
//! 壊れかけメディアでは「まずイメージを取り、イメージを [`TreeSource`] で展開する」
//! のが推奨ルート(PLAN.md 6章 4項)。デバイスは触るたびに劣化する。
//!
//! ```no_run
//! use ofr_copy::{Copier, CopyOptions, MountSource};
//!
//! let source = MountSource::new("/Volumes/USB");
//! let report = Copier::new(&source, "/Volumes/Backup/mirror")
//!     .with_options(CopyOptions { retries: 5, ..CopyOptions::default() })
//!     .with_file_done(|f| println!("{} {}", f.status, f.source))
//!     .run()?;
//!
//! // 全ファイルの成否は JSON、人間向けサマリはテキストで宛先に残る。
//! report.write_to_dir(std::path::Path::new("/Volumes/Backup/mirror"))?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # 壊れかけメディアでの振る舞い
//!
//! 1 ファイルの一部が読めなくても止まらない。リトライ(指数バックオフ)のうえ、
//! それでも読めない部分はゼロで埋めて残りを書き出し、埋めた量を
//! [`FileResult::missing`] に記録する。「読めた分は保存する」が復旧ソフトとして
//! 正しい振る舞いで、開けなくなったファイルは Phase 5 の修復モジュールへ回す。
//!
//! # 安全原則
//!
//! - 復旧元へは書き込まない。[`ofr_device::Device`] に書き込み経路がなく、
//!   [`MountSource`] も読み込みしか行わない(PLAN.md 6章 1項)
//! - 宛先が復旧元の中にある場合は始める前に弾く
//!   ([`CopyError::DestinationInsideSource`])。復旧元と宛先が同じ*デバイス*かの
//!   判定は呼び出し側(CLI / GUI)の仕事(同 2項)
//! - シンボリックリンクは辿らない。リンク先がデバイスの外に出ると、
//!   関係のないファイルまでコピーしてしまう

#![deny(unsafe_code)]

mod copier;
mod error;
mod mount;
mod options;
mod progress;
mod report;
mod source;
mod tree;

pub use copier::Copier;
pub use error::{CopyError, Result};
pub use mount::MountSource;
pub use options::{CopyOptions, ExistingFile};
pub use progress::{CopyProgress, FileDoneFn, ProgressFn};
pub use report::{CopyReport, CopyStatus, CopySummary, FileResult, REPORT_JSON, REPORT_TEXT};
pub use source::{CopyItem, CopySource};
pub use tree::TreeSource;

/// 1 ファイルの読み出し結果。[`ofr_fs`] と同じ型を使う。
pub use ofr_fs::ExtractStats;
