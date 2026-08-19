//! 項目の絞り込み。`ofr scan` と `ofr restore` で同じ条件を使う。
//!
//! 判定そのものは [`ofr_core::filter`] にあり(GUI の検索欄と同じ挙動になる)、
//! ここは clap の引数型との橋渡しだけを持つ。

use ofr_fs::{EntryStatus, RecoveredEntry};

/// `--status` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StatusChoice {
    /// 生きている項目。
    Intact,
    /// 削除済み。
    Deleted,
    /// ルートから辿れない場所で見つかったもの。
    Orphaned,
    /// 壊れているもの。
    Damaged,
}

impl From<StatusChoice> for EntryStatus {
    fn from(value: StatusChoice) -> Self {
        match value {
            StatusChoice::Intact => EntryStatus::Intact,
            StatusChoice::Deleted => EntryStatus::Deleted,
            StatusChoice::Orphaned => EntryStatus::Orphaned,
            StatusChoice::Damaged => EntryStatus::Damaged,
        }
    }
}

/// 絞り込み条件。空なら全部通す。
#[derive(Debug, Default, Clone)]
pub struct Filter {
    inner: ofr_core::filter::Filter,
}

impl Filter {
    /// CLI の引数から組み立てる。
    pub fn new(include: Vec<String>, statuses: &[StatusChoice]) -> Self {
        Self {
            inner: ofr_core::filter::Filter {
                include,
                statuses: statuses.iter().copied().map(EntryStatus::from).collect(),
            },
        }
    }

    /// 条件が何も指定されていないか。
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// この項目を通すか。
    pub fn matches(&self, entry: &RecoveredEntry) -> bool {
        self.inner.matches(entry)
    }
}
