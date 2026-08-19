//! 32bit FAT 表の読み出し。
//!
//! FAT32 と exFAT は使い方こそ違うが、表の形(クラスタ番号を添字にした 32bit の
//! 配列)は同じなので、ここで共通に扱う。違いは有効ビット幅だけ:
//!
//! - FAT32: 下位 28bit が有効。終端は `0x0FFFFFF8` 以上、不良は `0x0FFFFFF7`
//! - exFAT: 32bit 全部が有効。終端は `0xFFFFFFFF`、不良は `0xFFFFFFF7`
//!
//! 壊れたボリュームでは輪(自分自身や既に通ったクラスタへ戻るチェーン)が普通に
//! できているので、チェーン追跡は必ず訪問済み集合と上限で止める。

use std::collections::HashSet;

use ofr_device::Device;

use crate::cache::WindowCache;

/// 空きクラスタを表す値。
pub const FREE: u32 = 0;

/// 最初の有効なクラスタ番号。0 と 1 は予約。
pub const FIRST_CLUSTER: u32 = 2;

/// FAT チェーンの追跡結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chain {
    /// 辿れたクラスタ。
    pub clusters: Vec<u32>,
    /// 上限に達して打ち切った。
    pub truncated: bool,
    /// 輪になっていた。
    pub looped: bool,
    /// 不良クラスタ・範囲外など、正常でない値で終わった。
    pub broken: bool,
}

/// 32bit の FAT 表。
pub struct Fat32Table<'a> {
    cache: WindowCache<'a>,
    /// 有効ビットのマスク。
    mask: u32,
    /// 表に入っている項目数(= クラスタ番号の上限 + 1)。
    entries: u32,
}

impl<'a> Fat32Table<'a> {
    /// `offset` から `len` バイトを FAT 表として扱う。
    ///
    /// `mask` は有効ビット(FAT32 は `0x0FFF_FFFF`、exFAT は `0xFFFF_FFFF`)。
    pub fn new(device: &'a dyn Device, offset: u64, len: u64, mask: u32) -> Self {
        let entries = (len / 4).min(u32::MAX as u64) as u32;
        Self {
            cache: WindowCache::new(device, offset, len),
            mask,
            entries,
        }
    }

    /// 表に入っている項目数。
    pub fn entries(&self) -> u32 {
        self.entries
    }

    /// 読み込みに失敗した窓の数。FAT 自体が壊れている目安になる。
    pub fn read_failures(&self) -> u32 {
        self.cache.failures()
    }

    /// クラスタ 1 個分の値。範囲外なら `None`。
    pub fn entry(&self, cluster: u32) -> Option<u32> {
        if cluster >= self.entries {
            return None;
        }
        self.cache.u32_at(cluster as u64 * 4).map(|v| v & self.mask)
    }

    /// 空きクラスタか。範囲外は `false`。
    pub fn is_free(&self, cluster: u32) -> bool {
        self.entry(cluster) == Some(FREE)
    }

    /// 終端マークか。
    pub fn is_end(&self, value: u32) -> bool {
        value >= (self.mask & 0xFFFF_FFF8)
    }

    /// 不良クラスタのマークか。
    pub fn is_bad(&self, value: u32) -> bool {
        value == (self.mask & 0xFFFF_FFF7)
    }

    /// 次のクラスタ。終端・不良・範囲外なら `None`。
    pub fn next(&self, cluster: u32) -> Option<u32> {
        let value = self.entry(cluster)?;
        if value < FIRST_CLUSTER
            || value >= self.entries
            || self.is_end(value)
            || self.is_bad(value)
        {
            return None;
        }
        Some(value)
    }

    /// `start` からチェーンを辿る。`max` 個で打ち切る。
    pub fn chain(&self, start: u32, max: usize) -> Chain {
        let mut chain = Chain::default();
        if start < FIRST_CLUSTER || start >= self.entries || max == 0 {
            chain.broken = true;
            return chain;
        }

        let mut seen = HashSet::new();
        let mut cluster = start;
        loop {
            if !seen.insert(cluster) {
                chain.looped = true;
                break;
            }
            chain.clusters.push(cluster);
            if chain.clusters.len() >= max {
                chain.truncated = self.next(cluster).is_some();
                break;
            }
            let Some(value) = self.entry(cluster) else {
                chain.broken = true;
                break;
            };
            if self.is_end(value) {
                break;
            }
            if value < FIRST_CLUSTER || value >= self.entries || self.is_bad(value) {
                chain.broken = true;
                break;
            }
            cluster = value;
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;

    /// クラスタ番号 → 値の並びを、そのまま FAT 表のバイト列にする。
    fn table(values: &[u32]) -> MockDevice {
        let mut data = Vec::new();
        for v in values {
            data.extend_from_slice(&v.to_le_bytes());
        }
        data.resize(4096, 0);
        MockDevice::builder(4096).data(data).build()
    }

    #[test]
    fn follows_a_simple_chain() {
        // 2 → 3 → 7 → 終端
        let device = table(&[0x0FFF_FFF8, 0x0FFF_FFFF, 3, 7, 0, 0, 0, 0x0FFF_FFFF]);
        let fat = Fat32Table::new(&device, 0, 4096, 0x0FFF_FFFF);
        let chain = fat.chain(2, 100);
        assert_eq!(chain.clusters, vec![2, 3, 7]);
        assert!(!chain.looped && !chain.broken && !chain.truncated);
    }

    #[test]
    fn stops_on_loops() {
        // 2 → 3 → 2 ...
        let device = table(&[0, 0, 3, 2]);
        let fat = Fat32Table::new(&device, 0, 4096, 0x0FFF_FFFF);
        let chain = fat.chain(2, 100);
        assert_eq!(chain.clusters, vec![2, 3]);
        assert!(chain.looped);
    }

    #[test]
    fn stops_at_the_limit() {
        // 2 → 3 → 4 → 5 ...
        let device = table(&[0, 0, 3, 4, 5, 6]);
        let fat = Fat32Table::new(&device, 0, 4096, 0x0FFF_FFFF);
        let chain = fat.chain(2, 2);
        assert_eq!(chain.clusters, vec![2, 3]);
        assert!(chain.truncated);
    }

    #[test]
    fn marks_broken_chains() {
        // 2 → 不良クラスタ
        let device = table(&[0, 0, 0x0FFF_FFF7]);
        let fat = Fat32Table::new(&device, 0, 4096, 0x0FFF_FFFF);
        let chain = fat.chain(2, 100);
        assert_eq!(chain.clusters, vec![2]);
        assert!(chain.broken);

        // 0 (空き) で終わるチェーンも壊れている。削除済みファイルの
        // チェーンは解放されているので、この形になる。
        let device = table(&[0, 0, 0]);
        let fat = Fat32Table::new(&device, 0, 4096, 0x0FFF_FFFF);
        assert!(fat.chain(2, 100).broken);
    }

    #[test]
    fn exfat_uses_the_full_width() {
        let device = table(&[0xFFFF_FFF8, 0xFFFF_FFFF, 3, 0xFFFF_FFFF]);
        let fat = Fat32Table::new(&device, 0, 4096, 0xFFFF_FFFF);
        assert_eq!(fat.chain(2, 100).clusters, vec![2, 3]);
        assert!(fat.is_end(0xFFFF_FFFF));
        assert!(fat.is_bad(0xFFFF_FFF7));
    }
}
