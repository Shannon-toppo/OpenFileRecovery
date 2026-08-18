//! 領域マップ。デバイスのどこが「未試行/取得済み/不良」かを保持する。
//!
//! GNU ddrescue の mapfile と同じ状態を持つ(PLAN.md 5.2)。
//! 全域を隙間なく覆う、位置順に並んだブロックの列として表現する。

use std::fmt;

/// 領域の状態。文字表現は ddrescue の mapfile と互換。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockStatus {
    /// まだ読んでいない (`?`)。
    NonTried,
    /// 読んで失敗し、まだ端を詰めていない (`*`)。
    NonTrimmed,
    /// 端を詰めたが、まだセクタ単位で総当たりしていない (`/`)。
    NonScraped,
    /// セクタ単位でも読めなかった (`-`)。
    BadSector,
    /// 読めた (`+`)。
    Finished,
}

impl BlockStatus {
    /// mapfile 上の 1 文字表現。
    pub fn as_char(self) -> char {
        match self {
            BlockStatus::NonTried => '?',
            BlockStatus::NonTrimmed => '*',
            BlockStatus::NonScraped => '/',
            BlockStatus::BadSector => '-',
            BlockStatus::Finished => '+',
        }
    }

    /// mapfile の 1 文字表現から復元する。
    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '?' => BlockStatus::NonTried,
            '*' => BlockStatus::NonTrimmed,
            '/' => BlockStatus::NonScraped,
            '-' => BlockStatus::BadSector,
            '+' => BlockStatus::Finished,
            _ => return None,
        })
    }

    /// 回収済みか。
    pub fn is_rescued(self) -> bool {
        self == BlockStatus::Finished
    }
}

impl fmt::Display for BlockStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BlockStatus::NonTried => "未試行",
            BlockStatus::NonTrimmed => "未トリム",
            BlockStatus::NonScraped => "未スクレイプ",
            BlockStatus::BadSector => "不良",
            BlockStatus::Finished => "取得済み",
        })
    }
}

/// 同じ状態が続く 1 区間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// 開始オフセット。
    pub pos: u64,
    /// 長さ(バイト)。
    pub size: u64,
    /// 状態。
    pub status: BlockStatus,
}

impl Block {
    /// 終端オフセット(この位置は含まない)。
    pub fn end(&self) -> u64 {
        self.pos + self.size
    }
}

/// デバイス全域を覆うブロックの列。
///
/// 常に「位置順・隙間なし・重なりなし・隣接する同状態はマージ済み」に保たれる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockList {
    total: u64,
    blocks: Vec<Block>,
}

impl BlockList {
    /// 全域を未試行として作る。
    pub fn new(total: u64) -> Self {
        let blocks = if total == 0 {
            Vec::new()
        } else {
            vec![Block {
                pos: 0,
                size: total,
                status: BlockStatus::NonTried,
            }]
        };
        Self { total, blocks }
    }

    /// 任意のブロック列から作る。
    ///
    /// 重なりは後勝ちで潰し、隙間は未試行で埋め、`total` までを覆う形に正規化する。
    /// mapfile を読み込むときに使う(手書きされた mapfile も受け付けるため)。
    pub fn from_blocks(total: u64, mut blocks: Vec<Block>) -> Self {
        blocks.retain(|b| b.size > 0 && b.pos < total);
        blocks.sort_by_key(|b| b.pos);

        let mut list = Self::new(total);
        for b in blocks {
            list.mark(b.pos, b.size, b.status);
        }
        list
    }

    /// 全長。
    pub fn total(&self) -> u64 {
        self.total
    }

    /// ブロック列。
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// 指定範囲の状態を書き換える。
    pub fn mark(&mut self, pos: u64, size: u64, status: BlockStatus) {
        if size == 0 || pos >= self.total {
            return;
        }
        let start = pos;
        let end = pos.saturating_add(size).min(self.total);
        if start >= end {
            return;
        }

        let mut out: Vec<Block> = Vec::with_capacity(self.blocks.len() + 2);
        for b in std::mem::take(&mut self.blocks) {
            if b.end() <= start || b.pos >= end {
                out.push(b);
                continue;
            }
            if b.pos < start {
                out.push(Block {
                    pos: b.pos,
                    size: start - b.pos,
                    status: b.status,
                });
            }
            if b.end() > end {
                out.push(Block {
                    pos: end,
                    size: b.end() - end,
                    status: b.status,
                });
            }
        }
        out.push(Block {
            pos: start,
            size: end - start,
            status,
        });
        out.sort_by_key(|b| b.pos);
        self.blocks = out;
        self.merge();
    }

    /// 指定位置の状態。範囲外なら `None`。
    pub fn status_at(&self, pos: u64) -> Option<BlockStatus> {
        self.blocks
            .iter()
            .find(|b| b.pos <= pos && pos < b.end())
            .map(|b| b.status)
    }

    /// その状態のブロックだけを取り出す。
    ///
    /// パス実行中はマップを書き換えるので、パス開始時にこれで取った
    /// スナップショットに対して処理を進める。
    pub fn ranges_with(&self, status: BlockStatus) -> Vec<Block> {
        self.blocks
            .iter()
            .copied()
            .filter(|b| b.status == status)
            .collect()
    }

    /// その状態の合計バイト数。
    pub fn bytes_with(&self, status: BlockStatus) -> u64 {
        self.blocks
            .iter()
            .filter(|b| b.status == status)
            .map(|b| b.size)
            .sum()
    }

    /// 取得済みバイト数。
    pub fn rescued(&self) -> u64 {
        self.bytes_with(BlockStatus::Finished)
    }

    /// まだ取得できていないバイト数(未試行 + 不良を含む)。
    pub fn remaining(&self) -> u64 {
        self.total - self.rescued()
    }

    /// 全域が取得済みか。
    pub fn is_complete(&self) -> bool {
        self.remaining() == 0
    }

    fn merge(&mut self) {
        let mut merged: Vec<Block> = Vec::with_capacity(self.blocks.len());
        for b in std::mem::take(&mut self.blocks) {
            match merged.last_mut() {
                Some(last) if last.status == b.status && last.end() == b.pos => {
                    last.size += b.size;
                }
                _ => merged.push(b),
            }
        }
        self.blocks = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_as_one_non_tried_block() {
        let list = BlockList::new(1000);
        assert_eq!(list.blocks().len(), 1);
        assert_eq!(list.bytes_with(BlockStatus::NonTried), 1000);
        assert_eq!(list.remaining(), 1000);
        assert!(!list.is_complete());
    }

    #[test]
    fn marking_splits_and_merges() {
        let mut list = BlockList::new(1000);
        list.mark(100, 100, BlockStatus::Finished);
        assert_eq!(list.blocks().len(), 3);

        // 隣を同じ状態にすると 1 つにまとまる。
        list.mark(200, 100, BlockStatus::Finished);
        assert_eq!(list.bytes_with(BlockStatus::Finished), 200);
        assert_eq!(
            list.blocks()[1],
            Block {
                pos: 100,
                size: 200,
                status: BlockStatus::Finished
            }
        );

        // 全域を塗れば 1 ブロックに戻る。
        list.mark(0, 1000, BlockStatus::Finished);
        assert_eq!(list.blocks().len(), 1);
        assert!(list.is_complete());
    }

    #[test]
    fn marking_inside_a_block_carves_the_middle() {
        let mut list = BlockList::new(1000);
        list.mark(0, 1000, BlockStatus::Finished);
        list.mark(400, 100, BlockStatus::BadSector);

        assert_eq!(list.blocks().len(), 3);
        assert_eq!(list.status_at(399), Some(BlockStatus::Finished));
        assert_eq!(list.status_at(400), Some(BlockStatus::BadSector));
        assert_eq!(list.status_at(500), Some(BlockStatus::Finished));
        assert_eq!(list.bytes_with(BlockStatus::BadSector), 100);
    }

    #[test]
    fn marks_are_clamped_to_the_device() {
        let mut list = BlockList::new(1000);
        list.mark(900, 500, BlockStatus::Finished);
        assert_eq!(list.rescued(), 100);
        list.mark(1000, 100, BlockStatus::Finished);
        assert_eq!(list.rescued(), 100);
        assert_eq!(list.status_at(1000), None);

        // 全ブロックの合計は常に全長と一致する。
        let sum: u64 = list.blocks().iter().map(|b| b.size).sum();
        assert_eq!(sum, 1000);
    }

    #[test]
    fn rebuilds_from_sparse_block_lists() {
        // 隙間のあるブロック列は未試行で埋められる。
        let list = BlockList::from_blocks(
            1000,
            vec![
                Block {
                    pos: 500,
                    size: 100,
                    status: BlockStatus::BadSector,
                },
                Block {
                    pos: 0,
                    size: 100,
                    status: BlockStatus::Finished,
                },
            ],
        );
        assert_eq!(list.total(), 1000);
        assert_eq!(list.bytes_with(BlockStatus::Finished), 100);
        assert_eq!(list.bytes_with(BlockStatus::BadSector), 100);
        assert_eq!(list.bytes_with(BlockStatus::NonTried), 800);
    }

    #[test]
    fn status_chars_round_trip() {
        for s in [
            BlockStatus::NonTried,
            BlockStatus::NonTrimmed,
            BlockStatus::NonScraped,
            BlockStatus::BadSector,
            BlockStatus::Finished,
        ] {
            assert_eq!(BlockStatus::from_char(s.as_char()), Some(s));
        }
        assert_eq!(BlockStatus::from_char('x'), None);
    }
}
