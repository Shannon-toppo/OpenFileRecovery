//! 項目の絞り込み。`ofr scan` と `ofr restore` で同じ条件を使う。

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

impl StatusChoice {
    fn matches(self, status: EntryStatus) -> bool {
        matches!(
            (self, status),
            (StatusChoice::Intact, EntryStatus::Intact)
                | (StatusChoice::Deleted, EntryStatus::Deleted)
                | (StatusChoice::Orphaned, EntryStatus::Orphaned)
                | (StatusChoice::Damaged, EntryStatus::Damaged)
        )
    }
}

/// 絞り込み条件。空なら全部通す。
#[derive(Debug, Default, Clone)]
pub struct Filter {
    /// パスまたはファイル名にマッチさせるパターン(`*` と `?` が使える)。
    pub include: Vec<String>,
    /// 状態。
    pub statuses: Vec<StatusChoice>,
}

impl Filter {
    /// 条件が何も指定されていないか。
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.statuses.is_empty()
    }

    /// この項目を通すか。
    pub fn matches(&self, entry: &RecoveredEntry) -> bool {
        if !self.statuses.is_empty() && !self.statuses.iter().any(|s| s.matches(entry.status)) {
            return false;
        }
        if !self.include.is_empty() {
            let hit = self.include.iter().any(|pattern| {
                if pattern.contains('/') {
                    glob(pattern, &entry.path)
                } else {
                    glob(pattern, &entry.name) || glob(pattern, &entry.path)
                }
            });
            if !hit {
                return false;
            }
        }
        true
    }
}

/// `*` と `?` だけの簡易グロブ。大文字小文字は区別しない。
///
/// FAT/exFAT のファイル名は大文字小文字を区別しないので、絞り込みも合わせる。
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();

    let (mut pi, mut ti) = (0usize, 0usize);
    // 直前に見た `*` の位置と、そのときのテキスト位置(後戻り用)。
    let (mut star, mut backtrack) = (None, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            backtrack = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            backtrack += 1;
            ti = backtrack;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use ofr_fs::{EntryKind, RecoveredEntry};

    use super::*;

    fn entry(path: &str, status: EntryStatus) -> RecoveredEntry {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut e = RecoveredEntry::new(name, EntryKind::File, status);
        e.path = path.to_string();
        e
    }

    #[test]
    fn matches_wildcards() {
        assert!(glob("*.jpg", "DSC00001.JPG"));
        assert!(glob("dsc*.jpg", "DSC00001.JPG"));
        assert!(glob("DSC0000?.JPG", "DSC00001.JPG"));
        assert!(glob("*", "なんでも"));
        assert!(glob("/DCIM/*/*.JPG", "/DCIM/100MSDCF/DSC00001.JPG"));
        assert!(!glob("*.png", "DSC00001.JPG"));
        assert!(!glob("DSC0000?.JPG", "DSC000012.JPG"));
        assert!(glob("報告書*", "報告書 2026.txt"));
    }

    #[test]
    fn filters_by_pattern_and_status() {
        let filter = Filter {
            include: vec!["*.jpg".to_string()],
            statuses: vec![StatusChoice::Deleted],
        };
        assert!(filter.matches(&entry("/a/b.jpg", EntryStatus::Deleted)));
        assert!(!filter.matches(&entry("/a/b.jpg", EntryStatus::Intact)));
        assert!(!filter.matches(&entry("/a/b.txt", EntryStatus::Deleted)));
    }

    #[test]
    fn empty_filter_passes_everything() {
        let filter = Filter::default();
        assert!(filter.is_empty());
        assert!(filter.matches(&entry("/a/b.jpg", EntryStatus::Damaged)));
    }
}
