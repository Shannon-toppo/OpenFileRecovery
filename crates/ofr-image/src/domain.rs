//! 読む範囲の限定と、先に確保する範囲(PLAN.md 11.4)。
//!
//! 死にかけの個体は残り時間が有限なので、**どこを先に読むか**が結果を左右する。
//! ここで持つのは「デバイスのどの範囲を対象にするか」だけの、状態を持たない
//! 範囲の集合。使い道は 2 つある。
//!
//! - [`ImageOptions::domain`](crate::ImageOptions::domain): この範囲の外は
//!   一切読まない。走査で選んだファイルのクラスタ範囲だけを取り直す用
//! - [`ImageOptions::priority`](crate::ImageOptions::priority): コピーパスの
//!   最初にこの範囲だけを掃く。ブートセクタ、FAT 表、ディレクトリ領域を
//!   先に確保しておくと、途中で死んでも構造が残る
//!
//! ddrescue の `--domain-mapfile` と同じく、mapfile の取得済み (`+`) 領域を
//! 範囲として読める([`Domain::from_map`])。向こうは人手で作らせるが、
//! こちらは FS パーサを持っているので走査結果から作れる(`ofr scan --domain-out`)。

use std::path::Path;

use crate::blocks::{Block, BlockStatus};
use crate::error::Result;
use crate::mapfile::MapFile;

/// 連続した 1 区間。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// 開始オフセット。
    pub pos: u64,
    /// 長さ(バイト)。
    pub size: u64,
}

impl Region {
    /// 終端オフセット(この位置は含まない)。
    pub fn end(&self) -> u64 {
        self.pos.saturating_add(self.size)
    }
}

/// 対象範囲の集合。
///
/// 常に「位置順・重なりなし・隣接済みはマージ済み」に保たれる。空の集合も作れるが、
/// [`ImageOptions::domain`](crate::ImageOptions::domain) に空を渡すと
/// 読む所が無くなるので、イメージング側が設定エラーとして弾く。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Domain {
    regions: Vec<Region>,
}

impl Domain {
    /// 範囲の並びから作る。重なりと隣接はまとめ、長さ 0 は捨てる。
    pub fn new(regions: impl IntoIterator<Item = (u64, u64)>) -> Self {
        let mut regions: Vec<Region> = regions
            .into_iter()
            .filter(|(_, size)| *size > 0)
            .map(|(pos, size)| Region { pos, size })
            .collect();
        regions.sort_by_key(|r| r.pos);

        let mut merged: Vec<Region> = Vec::with_capacity(regions.len());
        for r in regions {
            match merged.last_mut() {
                Some(last) if r.pos <= last.end() => {
                    let end = last.end().max(r.end());
                    last.size = end - last.pos;
                }
                _ => merged.push(r),
            }
        }
        Self { regions: merged }
    }

    /// mapfile の取得済み (`+`) 領域を対象範囲とする(ddrescue の `--domain-mapfile` 相当)。
    pub fn from_map(map: &MapFile) -> Self {
        Self::new(
            map.blocks
                .blocks()
                .iter()
                .filter(|b| b.status == BlockStatus::Finished)
                .map(|b| (b.pos, b.size)),
        )
    }

    /// mapfile を読んで対象範囲にする。
    pub fn load(path: &Path) -> Result<Self> {
        Ok(Self::from_map(&MapFile::load(path)?))
    }

    /// 対象範囲を取得済み (`+`)、それ以外を未試行 (`?`) とした mapfile にする。
    ///
    /// `ofr scan --domain-out` が書き出すのはこれ。そのまま
    /// `ofr image --domain` に渡せる。
    pub fn to_map(&self, total: u64) -> MapFile {
        let mut map = MapFile::new(total);
        for r in &self.regions {
            map.blocks.mark(r.pos, r.size, BlockStatus::Finished);
        }
        map
    }

    /// 範囲を 1 つ足す。重なりと隣接はまとめる。
    pub fn insert(&mut self, pos: u64, size: u64) {
        if size == 0 {
            return;
        }
        let merged = Self::new(
            self.regions
                .iter()
                .map(|r| (r.pos, r.size))
                .chain(std::iter::once((pos, size))),
        );
        *self = merged;
    }

    /// 区間の並び(位置順)。
    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// 対象範囲の合計バイト数。
    pub fn total(&self) -> u64 {
        self.regions.iter().map(|r| r.size).sum()
    }

    /// 区間が 1 つもないか。
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// その位置が対象範囲に入っているか。
    pub fn contains(&self, pos: u64) -> bool {
        self.regions.iter().any(|r| r.pos <= pos && pos < r.end())
    }

    /// セクタ境界まで外側へ広げ、`limit` で切り詰めた集合を返す。
    ///
    /// クラスタ範囲はセクタ境界に乗っているとは限らない(先頭がセクタの途中から
    /// 始まる FS もある)。読み込みはセクタ単位なので、**外側へ**丸めて
    /// 取りこぼさないようにする。
    pub fn aligned(&self, sector: u64, limit: u64) -> Self {
        let sector = sector.max(1);
        Self::new(self.regions.iter().filter_map(|r| {
            let start = r.pos / sector * sector;
            let end = r.end().div_ceil(sector).saturating_mul(sector).min(limit);
            (start < end).then(|| (start, end - start))
        }))
    }

    /// 2 つの集合の共通部分。
    pub fn intersect(&self, other: &Domain) -> Self {
        let mut out = Vec::new();
        for r in &self.regions {
            for o in &other.regions {
                let start = r.pos.max(o.pos);
                let end = r.end().min(o.end());
                if start < end {
                    out.push((start, end - start));
                }
            }
        }
        Self::new(out)
    }

    /// ブロックのうち対象範囲に入っている部分だけを切り出す。状態は引き継ぐ。
    pub fn clip(&self, block: Block) -> Vec<Block> {
        self.regions
            .iter()
            .filter_map(|r| {
                let start = block.pos.max(r.pos);
                let end = block.end().min(r.end());
                (start < end).then(|| Block {
                    pos: start,
                    size: end - start,
                    status: block.status,
                })
            })
            .collect()
    }

    /// [`Domain::clip`] をブロック列に適用する。
    pub fn clip_all(&self, blocks: &[Block]) -> Vec<Block> {
        blocks.iter().flat_map(|b| self.clip(*b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_and_adjacent_regions() {
        let d = Domain::new([(100, 50), (0, 10), (10, 10), (140, 20), (300, 0)]);
        assert_eq!(
            d.regions(),
            [Region { pos: 0, size: 20 }, Region { pos: 100, size: 60 },]
        );
        assert_eq!(d.total(), 80);
        assert!(d.contains(0));
        assert!(!d.contains(20));
        assert!(d.contains(159));
        assert!(!d.contains(160));
    }

    #[test]
    fn aligns_outward_to_sector_boundaries() {
        // 取りこぼさないよう、始まりは切り下げ、終わりは切り上げる。
        let d = Domain::new([(600, 100)]).aligned(512, 4096);
        assert_eq!(
            d.regions(),
            [Region {
                pos: 512,
                size: 512
            }]
        );
        // デバイス長を超える部分は落とす。
        let d = Domain::new([(3900, 500)]).aligned(512, 4096);
        assert_eq!(
            d.regions(),
            [Region {
                pos: 3584,
                size: 512
            }]
        );
    }

    #[test]
    fn clips_blocks_to_the_domain() {
        let d = Domain::new([(0, 100), (200, 100)]);
        let clipped = d.clip(Block {
            pos: 50,
            size: 200,
            status: BlockStatus::NonTried,
        });
        assert_eq!(
            clipped,
            [
                Block {
                    pos: 50,
                    size: 50,
                    status: BlockStatus::NonTried
                },
                Block {
                    pos: 200,
                    size: 50,
                    status: BlockStatus::NonTried
                },
            ]
        );
    }

    #[test]
    fn round_trips_through_a_mapfile() {
        let d = Domain::new([(512, 1024), (4096, 512)]);
        let map = d.to_map(8192);
        assert_eq!(Domain::from_map(&map), d);
        let text = map.to_text();
        assert_eq!(Domain::from_map(&MapFile::parse(&text).unwrap()), d);
    }

    #[test]
    fn inserts_and_merges() {
        let mut d = Domain::new([(0, 100)]);
        d.insert(100, 50);
        d.insert(300, 50);
        d.insert(0, 0);
        assert_eq!(d, Domain::new([(0, 150), (300, 50)]));
    }

    #[test]
    fn intersects_two_domains() {
        let a = Domain::new([(0, 100), (200, 100)]);
        let b = Domain::new([(50, 200)]);
        assert_eq!(a.intersect(&b), Domain::new([(50, 50), (200, 50)]));
    }
}
