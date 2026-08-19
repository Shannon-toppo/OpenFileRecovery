//! 解析結果のツリーからのコピー。
//!
//! マウントできないデバイスと、`ofr image` で取ったイメージがこの経路になる。
//! 壊れかけメディアではこちらが推奨ルート(PLAN.md 5.5 の 2 番)。
//!
//! 読み出しは [`ofr_fs::extract`] に任せる。復元(`ofr restore`)と同じ実装を
//! 通るので、「読めない所はリトライしてからゼロで埋め、先へ進む」振る舞いも同じ。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ofr_device::Device;
use ofr_fs::{
    EntryStatus, ExtractStats, FileTree, RecoveredEntry,
    extract::{self, sanitize_component},
};

use crate::error::Result;
use crate::options::CopyOptions;
use crate::source::{CopyItem, CopySource};

/// 解析済みツリーをそのままコピーする元。
pub struct TreeSource<'a> {
    device: &'a dyn Device,
    tree: &'a FileTree,
    label: String,
    statuses: Vec<EntryStatus>,
}

impl<'a> TreeSource<'a> {
    /// 生きているファイル(`Intact`)だけをコピーする元を作る。
    ///
    /// 削除済み・孤立の項目も欲しい場合は [`TreeSource::with_statuses`] で足す。
    /// 選んで復元したいだけなら、コピーではなく `ofr restore` の仕事。
    pub fn new(device: &'a dyn Device, tree: &'a FileTree) -> Self {
        Self {
            device,
            tree,
            label: String::new(),
            statuses: vec![EntryStatus::Intact],
        }
    }

    /// レポートに書く復旧元の名前を付ける。
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// コピーする項目の状態を指定する。
    pub fn with_statuses(mut self, statuses: impl IntoIterator<Item = EntryStatus>) -> Self {
        self.statuses = statuses.into_iter().collect();
        self
    }

    /// 全ての状態(削除済み・孤立・破損を含む)をコピーする。
    pub fn with_all_statuses(self) -> Self {
        self.with_statuses([
            EntryStatus::Intact,
            EntryStatus::Deleted,
            EntryStatus::Orphaned,
            EntryStatus::Damaged,
        ])
    }

    fn wanted(&self, entry: &RecoveredEntry) -> bool {
        self.statuses.contains(&entry.status)
    }
}

impl CopySource for TreeSource<'_> {
    fn label(&self) -> String {
        if self.label.is_empty() {
            self.device.info().id.clone()
        } else {
            self.label.clone()
        }
    }

    fn collect(&self, cancel: &AtomicBool) -> Result<Vec<CopyItem>> {
        let mut items = Vec::new();
        // depth_first は親が子より先に来るので、そのままディレクトリ作成順になる。
        for (_, id) in self.tree.depth_first() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let Some(entry) = self.tree.get(id) else {
                continue;
            };
            if !self.wanted(entry) {
                continue;
            }
            items.push(CopyItem {
                path: entry.path.clone(),
                components: entry
                    .path
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(sanitize_component)
                    .collect(),
                kind: entry.kind,
                size: entry.recoverable_bytes(),
                modified: entry.times.modified.and_then(|t| t.to_system_time()),
                status: entry.status,
                id: id as u64,
            });
        }
        Ok(items)
    }

    fn copy_file(
        &self,
        item: &CopyItem,
        output: &Path,
        options: &CopyOptions,
    ) -> Result<ExtractStats> {
        let entry = self
            .tree
            .get(item.id as usize)
            .ok_or_else(|| ofr_fs::FsError::Unsupported(format!("項目 {} が消えた", item.path)))?;
        Ok(extract::extract_to_path(
            self.device,
            entry,
            output,
            &options.extract_options(),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;
    use ofr_fs::{EntryKind, Extent, RecoveredEntry};

    use super::*;

    fn tree() -> FileTree {
        let mut tree = FileTree::new();
        let dir = tree.push(
            RecoveredEntry::new("DCIM", EntryKind::Dir, EntryStatus::Intact),
            None,
        );
        let mut file = RecoveredEntry::new("a.jpg", EntryKind::File, EntryStatus::Intact);
        file.size = 64;
        file.extents = vec![Extent { offset: 0, len: 64 }];
        tree.push(file, Some(dir));
        let mut gone = RecoveredEntry::new("b.jpg", EntryKind::File, EntryStatus::Deleted);
        gone.size = 64;
        gone.extents = vec![Extent {
            offset: 512,
            len: 64,
        }];
        tree.push(gone, Some(dir));
        tree
    }

    #[test]
    fn collects_intact_entries_parents_first() {
        let device = MockDevice::patterned(4096);
        let tree = tree();
        let source = TreeSource::new(&device, &tree);
        let items = source.collect(&AtomicBool::new(false)).unwrap();

        let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, vec!["/DCIM", "/DCIM/a.jpg"]);
        assert!(items[0].is_dir());
        assert_eq!(items[1].components, vec!["DCIM", "a.jpg"]);
    }

    #[test]
    fn includes_deleted_entries_when_asked() {
        let device = MockDevice::patterned(4096);
        let tree = tree();
        let source = TreeSource::new(&device, &tree).with_all_statuses();
        let items = source.collect(&AtomicBool::new(false)).unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn copies_a_file_through_the_extractor() {
        let dir = tempfile::tempdir().unwrap();
        let device = MockDevice::patterned(4096);
        let tree = tree();
        let source = TreeSource::new(&device, &tree);
        let items = source.collect(&AtomicBool::new(false)).unwrap();

        let out = dir.path().join("a.jpg");
        let stats = source
            .copy_file(&items[1], &out, &CopyOptions::default())
            .unwrap();
        assert_eq!(stats.written, 64);
        assert!(stats.is_complete());
        assert_eq!(std::fs::read(&out).unwrap()[0], MockDevice::pattern_byte(0));
    }
}
