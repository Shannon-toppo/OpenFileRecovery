//! コピー元の抽象。
//!
//! PLAN.md 5.5 のコピーには読み出し経路が 2 つある:
//!
//! - OS にマウントされているデバイスは OS のファイル API で読む
//!   ([`MountSource`](crate::MountSource))
//! - マウントできない(が生読みはできる)デバイスと、取得済みイメージは
//!   ofr-fat / ofr-exfat の解析結果を辿って直読みする
//!   ([`TreeSource`](crate::TreeSource))
//!
//! どちらの経路でも宛先にできるミラーツリーは同じものになる。
//! [`Copier`](crate::Copier) は両者をこの trait 越しに扱うので、
//! 経路の違いを知らない。

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use ofr_fs::{EntryKind, EntryStatus, ExtractStats};

use crate::error::Result;
use crate::options::CopyOptions;

/// コピーする 1 項目(ファイルまたはディレクトリ)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyItem {
    /// 復旧元でのパス(`/` 区切り)。表示とレポートに使う。
    pub path: String,
    /// 宛先に作る相対パスの要素。OS が受け付ける形に直してある。
    pub components: Vec<String>,
    /// 種別。
    pub kind: EntryKind,
    /// サイズ。ディレクトリは 0。
    pub size: u64,
    /// 更新日時。分からなければ `None`。
    pub modified: Option<SystemTime>,
    /// 状態。マウント経由のコピーは常に [`EntryStatus::Intact`]。
    pub status: EntryStatus,
    /// 復旧元側でこの項目を引くための番号。ソース実装が自由に使う。
    pub id: u64,
}

impl CopyItem {
    /// ディレクトリか。
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }
}

/// コピー元。
///
/// 実装は [`TreeSource`](crate::TreeSource) と [`MountSource`](crate::MountSource)。
pub trait CopySource {
    /// レポートに書く復旧元の名前。
    fn label(&self) -> String;

    /// コピーする項目を集める。親が子より先に来る順で返すこと。
    fn collect(&self, cancel: &AtomicBool) -> Result<Vec<CopyItem>>;

    /// 1 ファイルを `output` へ書き出す。
    ///
    /// 読めない部分があっても、[`CopyOptions::zero_fill`] が真なら読めた分を
    /// 書き出して [`ExtractStats::missing`] に埋めた量を記録する。
    /// 戻り値が `Err` になるのは、そのファイルを 1 バイトも救えないときだけ。
    fn copy_file(
        &self,
        item: &CopyItem,
        output: &Path,
        options: &CopyOptions,
    ) -> Result<ExtractStats>;

    /// 宛先として使ってよいか確かめる。
    ///
    /// 既定では何も見ない。自分自身の中へコピーされうる実装だけが上書きする。
    fn check_destination(&self, _dest: &Path) -> Result<()> {
        Ok(())
    }

    /// 収集中に気付いたこと(飛ばしたシンボリックリンクなど)。
    fn warnings(&self) -> Vec<String> {
        Vec::new()
    }
}
