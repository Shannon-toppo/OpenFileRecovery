//! 解析結果のツリー。
//!
//! 走査中に見つかった項目を順に積み、親子関係だけを持つ。パスは追加時に
//! 親から組み立てる。フォーマット後の走査では「後から見つかった親の下へ
//! 既存の枝を移す」ことがあるので([`FileTree::reparent`])、パスは付け替え時に
//! 部分木ごと振り直す。

use std::time::Duration;

use crate::entry::{EntryKind, EntryStatus, RecoveredEntry};

/// [`FileTree`] 内の項目 ID。
pub type EntryId = usize;

/// 走査結果の統計。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// 見つかったディレクトリ数。
    pub dirs: usize,
    /// 見つかったファイル数。
    pub files: usize,
    /// 削除済みの項目数。
    pub deleted: usize,
    /// 孤立していた項目数。
    pub orphaned: usize,
    /// 破損扱いの項目数。
    pub damaged: usize,
    /// 走査したクラスタ数。
    pub clusters_scanned: u64,
    /// 上限に達して打ち切ったか。
    pub truncated: bool,
    /// キャンセルされたか。
    pub cancelled: bool,
    /// 所要時間。
    pub elapsed: Duration,
}

/// 解析で見つかった項目のツリー。
#[derive(Debug, Clone, Default)]
pub struct FileTree {
    entries: Vec<RecoveredEntry>,
    children: Vec<Vec<EntryId>>,
    roots: Vec<EntryId>,
    /// 走査中に記録した警告(壊れていてスキップした項目など)。
    pub warnings: Vec<String>,
    /// 統計。
    pub stats: ScanStats,
}

impl FileTree {
    /// 空のツリー。
    pub fn new() -> Self {
        Self::default()
    }

    /// 項目を追加して ID を返す。`id` / `parent` / `path` はここで埋める。
    pub fn push(&mut self, mut entry: RecoveredEntry, parent: Option<EntryId>) -> EntryId {
        let id = self.entries.len();
        entry.id = id;
        entry.parent = parent.filter(|p| *p < id);
        entry.path = self.build_path(entry.parent, &entry.name);

        match entry.kind {
            EntryKind::Dir => self.stats.dirs += 1,
            EntryKind::File => self.stats.files += 1,
        }
        match entry.status {
            EntryStatus::Deleted => self.stats.deleted += 1,
            EntryStatus::Orphaned => self.stats.orphaned += 1,
            EntryStatus::Damaged => self.stats.damaged += 1,
            EntryStatus::Intact => {}
        }

        match entry.parent {
            Some(p) => self.children[p].push(id),
            None => self.roots.push(id),
        }
        self.entries.push(entry);
        self.children.push(Vec::new());
        id
    }

    /// 警告を記録する。
    pub fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        tracing::debug!("{message}");
        self.warnings.push(message);
    }

    /// 全項目。
    pub fn entries(&self) -> &[RecoveredEntry] {
        &self.entries
    }

    /// ID で引く。
    pub fn get(&self, id: EntryId) -> Option<&RecoveredEntry> {
        self.entries.get(id)
    }

    /// ID で引く(可変)。
    pub fn get_mut(&mut self, id: EntryId) -> Option<&mut RecoveredEntry> {
        self.entries.get_mut(id)
    }

    /// ルート直下の項目。
    pub fn roots(&self) -> &[EntryId] {
        &self.roots
    }

    /// 子項目。
    pub fn children(&self, id: EntryId) -> &[EntryId] {
        self.children.get(id).map_or(&[], |v| v.as_slice())
    }

    /// 項目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 空か。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 部分木を別の親の下へ移す。
    ///
    /// 孤立ディレクトリとして拾った枝の親が、あとから見つかったときに使う。
    /// 移動先が自分自身の子孫なら何もしない(輪を作らない)。
    pub fn reparent(&mut self, id: EntryId, new_parent: EntryId) {
        if id == new_parent || id >= self.entries.len() || new_parent >= self.entries.len() {
            return;
        }
        if self.is_ancestor(id, new_parent) {
            return;
        }

        match self.entries[id].parent {
            Some(old) => self.children[old].retain(|c| *c != id),
            None => self.roots.retain(|c| *c != id),
        }
        self.entries[id].parent = Some(new_parent);
        self.children[new_parent].push(id);
        self.rebuild_paths(id);
    }

    /// 名前を変えてパスを振り直す。
    pub fn rename(&mut self, id: EntryId, name: impl Into<String>) {
        if id >= self.entries.len() {
            return;
        }
        self.entries[id].name = name.into();
        self.rebuild_paths(id);
    }

    /// 深さ優先で `(深さ, ID)` を並べる。表示用。
    pub fn depth_first(&self) -> Vec<(usize, EntryId)> {
        let mut out = Vec::with_capacity(self.entries.len());
        let mut stack: Vec<(usize, EntryId)> = self.roots.iter().rev().map(|&id| (0, id)).collect();
        while let Some((depth, id)) = stack.pop() {
            out.push((depth, id));
            for &child in self.children(id).iter().rev() {
                stack.push((depth + 1, child));
            }
        }
        out
    }

    fn build_path(&self, parent: Option<EntryId>, name: &str) -> String {
        match parent.and_then(|p| self.entries.get(p)) {
            Some(p) => format!("{}/{}", p.path, name),
            None => format!("/{name}"),
        }
    }

    fn is_ancestor(&self, ancestor: EntryId, mut id: EntryId) -> bool {
        let mut guard = 0;
        while let Some(parent) = self.entries[id].parent {
            if parent == ancestor {
                return true;
            }
            id = parent;
            guard += 1;
            if guard > self.entries.len() {
                return true; // 壊れた親子関係。安全側に倒す。
            }
        }
        false
    }

    fn rebuild_paths(&mut self, root: EntryId) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let parent = self.entries[id].parent;
            let path = self.build_path(parent, &self.entries[id].name.clone());
            self.entries[id].path = path;
            stack.extend_from_slice(&self.children[id]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    fn dir(name: &str) -> RecoveredEntry {
        RecoveredEntry::new(name, EntryKind::Dir, EntryStatus::Intact)
    }

    fn file(name: &str) -> RecoveredEntry {
        RecoveredEntry::new(name, EntryKind::File, EntryStatus::Deleted)
    }

    #[test]
    fn builds_paths_from_parents() {
        let mut tree = FileTree::new();
        let d = tree.push(dir("DCIM"), None);
        let sub = tree.push(dir("100MSDCF"), Some(d));
        let f = tree.push(file("DSC00001.JPG"), Some(sub));

        assert_eq!(tree.get(d).unwrap().path, "/DCIM");
        assert_eq!(tree.get(f).unwrap().path, "/DCIM/100MSDCF/DSC00001.JPG");
        assert_eq!(tree.roots(), &[d]);
        assert_eq!(tree.children(d), &[sub]);
        assert_eq!(tree.stats.dirs, 2);
        assert_eq!(tree.stats.files, 1);
        assert_eq!(tree.stats.deleted, 1);
    }

    #[test]
    fn reparenting_moves_the_whole_subtree() {
        let mut tree = FileTree::new();
        let orphan = tree.push(dir("dir_00000123"), None);
        let child = tree.push(file("a.txt"), Some(orphan));
        let parent = tree.push(dir("PHOTOS"), None);

        tree.reparent(orphan, parent);
        tree.rename(orphan, "TRIP");

        assert_eq!(tree.get(orphan).unwrap().path, "/PHOTOS/TRIP");
        assert_eq!(tree.get(child).unwrap().path, "/PHOTOS/TRIP/a.txt");
        assert_eq!(tree.roots(), &[parent]);
    }

    #[test]
    fn refuses_to_create_cycles() {
        let mut tree = FileTree::new();
        let a = tree.push(dir("a"), None);
        let b = tree.push(dir("b"), Some(a));
        tree.reparent(a, b);
        assert_eq!(tree.get(a).unwrap().parent, None);
        assert_eq!(tree.get(b).unwrap().path, "/a/b");
    }

    #[test]
    fn walks_depth_first() {
        let mut tree = FileTree::new();
        let a = tree.push(dir("a"), None);
        let a1 = tree.push(file("a1"), Some(a));
        let b = tree.push(dir("b"), None);
        assert_eq!(tree.depth_first(), vec![(0, a), (1, a1), (0, b)]);
    }
}
