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

    /// 深刻度。大きいほど悪い。
    ///
    /// 帯グラフ用に領域をまとめるとき、1 区間に複数の状態が混ざったら
    /// 深刻なほうを残す。復旧作業で見たいのは「どこが読めていないか」なので、
    /// 取得済みで不良を隠さない。
    fn severity(self) -> u8 {
        match self {
            BlockStatus::Finished => 0,
            BlockStatus::NonTried => 1,
            BlockStatus::NonTrimmed => 2,
            BlockStatus::NonScraped => 3,
            BlockStatus::BadSector => 4,
        }
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

    /// 帯グラフ用に、区間数が `max` 以下になるまで間引いたマップ。
    ///
    /// GUI は進捗イベントのたびにこれを描く(PLAN.md 5.2)。不良セクタが
    /// 1 個だけの区間も潰れて消えないように、まとめるときは深刻なほうを残す。
    /// 元から `max` 以下ならそのまま返す。
    pub fn downsample(&self, max: usize) -> Vec<Block> {
        if max == 0 || self.total == 0 {
            return Vec::new();
        }
        if self.blocks.len() <= max {
            return self.blocks.clone();
        }

        // 全域を max 等分し、各バケツに重なるブロックのうち最も深刻な状態を採る。
        let mut buckets: Vec<Option<BlockStatus>> = vec![None; max];
        let width = self.total as f64 / max as f64;
        for b in &self.blocks {
            let first = ((b.pos as f64 / width) as usize).min(max - 1);
            let last = (((b.end().saturating_sub(1)) as f64 / width) as usize).min(max - 1);
            for slot in &mut buckets[first..=last] {
                match slot {
                    Some(current) if current.severity() >= b.status.severity() => {}
                    other => *other = Some(b.status),
                }
            }
        }

        // 同じ状態が続くバケツは 1 区間にまとめる。
        let mut out: Vec<Block> = Vec::new();
        for (i, status) in buckets.into_iter().enumerate() {
            let status = status.unwrap_or(BlockStatus::NonTried);
            let pos = (i as f64 * width) as u64;
            let end = if i + 1 == max {
                self.total
            } else {
                ((i + 1) as f64 * width) as u64
            };
            match out.last_mut() {
                Some(last) if last.status == status => last.size = end - last.pos,
                _ => out.push(Block {
                    pos,
                    size: end.saturating_sub(pos),
                    status,
                }),
            }
        }
        out
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
    fn downsampling_keeps_the_map_short() {
        let mut list = BlockList::new(1_000_000);
        // 1000 バイトおきに不良を置いて、区間を大量に作る。
        for i in 0..500 {
            list.mark(i * 2000, 1000, BlockStatus::BadSector);
        }
        assert!(list.blocks().len() > 100);

        let map = list.downsample(64);
        assert!(map.len() <= 64, "{} 区間", map.len());
        assert_eq!(map.first().unwrap().pos, 0);
        assert_eq!(map.last().unwrap().end(), 1_000_000);
    }

    /// 潰した区間に不良が 1 個でも混じっていたら不良として見せる。
    /// 「読めていない場所」を取得済みで隠さないため。
    #[test]
    fn downsampling_keeps_the_worst_status() {
        let mut list = BlockList::new(1_000_000);
        list.mark(0, 1_000_000, BlockStatus::Finished);
        list.mark(500_000, 512, BlockStatus::BadSector);

        let map = list.downsample(8);
        assert!(map.iter().any(|b| b.status == BlockStatus::BadSector));
    }

    #[test]
    fn downsampling_a_short_map_changes_nothing() {
        let mut list = BlockList::new(1000);
        list.mark(0, 500, BlockStatus::Finished);
        assert_eq!(list.downsample(64), list.blocks());
    }

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
