//! 解析結果の中間表現(PLAN.md 5.3)。
//!
//! FAT32 と exFAT のどちらを解析しても、上位にはこの形で渡す。GUI はこれを
//! ツリー表示し、選ばれたものを [`extract`](crate::extract) が復元する。

use crate::time::Timestamps;

/// 項目の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// ファイル。
    File,
    /// ディレクトリ。
    Dir,
}

/// 項目の状態。GUI ではバッジとして出す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryStatus {
    /// 生きている項目。通常どおり読める。
    Intact,
    /// 削除済み。データが残っていれば復元できる。
    Deleted,
    /// ルートから辿れない場所で見つかった(フォーマット後の主力)。
    Orphaned,
    /// 構造が壊れていて、内容が正しい保証がない。
    Damaged,
}

impl EntryStatus {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            EntryStatus::Intact => "無傷",
            EntryStatus::Deleted => "削除済み",
            EntryStatus::Orphaned => "孤立",
            EntryStatus::Damaged => "破損",
        }
    }

    /// JSON などに出す機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            EntryStatus::Intact => "intact",
            EntryStatus::Deleted => "deleted",
            EntryStatus::Orphaned => "orphaned",
            EntryStatus::Damaged => "damaged",
        }
    }
}

impl std::fmt::Display for EntryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// デバイス上の連続した領域。
///
/// オフセットは解析対象デバイス(パーティションを切り出している場合はその先頭)
/// からのバイト数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// 開始オフセット。
    pub offset: u64,
    /// 長さ。
    pub len: u64,
}

impl Extent {
    /// 終端オフセット(この値自体は含まない)。
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
}

/// 復元結果の確からしさ。UI で「この項目は怪しい」と伝えるために使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryQuality {
    /// FAT チェーンが失われていたので、連続配置と仮定してクラスタを拾った。
    ///
    /// 断片化していたファイルはこの仮定で壊れる(PLAN.md 10章)。壊れた場合は
    /// Phase 5 の修復モジュールへ回す。
    pub contiguous_assumed: bool,
    /// 名前を完全には復元できていない(8.3 名の先頭 1 文字が失われている等)。
    pub name_partial: bool,
    /// 仮定した領域のうち、他のファイルに使われているクラスタの数。
    ///
    /// 0 より大きければ、そのクラスタは上書きされている可能性が高い。
    pub conflicting_clusters: u32,
    /// 記録されたサイズ分の領域を集めきれなかった。
    pub truncated: bool,
}

impl EntryQuality {
    /// 何か懸念があるか。
    pub fn has_concerns(&self) -> bool {
        self.contiguous_assumed
            || self.name_partial
            || self.conflicting_clusters > 0
            || self.truncated
    }
}

/// 復元候補の 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredEntry {
    /// [`FileTree`](crate::FileTree) 内での ID。
    pub id: usize,
    /// 親ディレクトリの ID。ルート直下なら `None`。
    pub parent: Option<usize>,
    /// 名前。
    pub name: String,
    /// ルートからのパス(`/` 区切り)。
    pub path: String,
    /// 種別。
    pub kind: EntryKind,
    /// ファイルサイズ。ディレクトリは 0。
    pub size: u64,
    /// 日時。
    pub times: Timestamps,
    /// 開始クラスタ番号(分かっていれば)。
    pub first_cluster: Option<u32>,
    /// データの実体位置。復元はここを順に読んで書き出す。
    pub extents: Vec<Extent>,
    /// 状態。
    pub status: EntryStatus,
    /// 確からしさ。
    pub quality: EntryQuality,
}

impl RecoveredEntry {
    /// 名前と種別だけを埋めた項目を作る。残りは呼び出し側が設定する。
    pub fn new(name: impl Into<String>, kind: EntryKind, status: EntryStatus) -> Self {
        Self {
            id: 0,
            parent: None,
            name: name.into(),
            path: String::new(),
            kind,
            size: 0,
            times: Timestamps::default(),
            first_cluster: None,
            extents: Vec::new(),
            status,
            quality: EntryQuality::default(),
        }
    }

    /// ディレクトリか。
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// 集められた領域の合計バイト数。
    pub fn available_bytes(&self) -> u64 {
        self.extents.iter().map(|e| e.len).sum()
    }

    /// 実際に書き出されるバイト数(記録サイズと集めた領域の小さいほう)。
    pub fn recoverable_bytes(&self) -> u64 {
        self.available_bytes().min(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_recoverable_bytes() {
        let mut e = RecoveredEntry::new("a.txt", EntryKind::File, EntryStatus::Deleted);
        e.size = 5000;
        // クラスタ 2 個分 (8KiB) を拾ったが、サイズは 5000 バイト。
        e.extents.push(Extent {
            offset: 0,
            len: 8192,
        });
        assert_eq!(e.available_bytes(), 8192);
        assert_eq!(e.recoverable_bytes(), 5000);

        // 1 クラスタしか拾えなければ、書き出せるのはその分だけ。
        e.extents = vec![Extent {
            offset: 0,
            len: 4096,
        }];
        assert_eq!(e.recoverable_bytes(), 4096);
    }
}
