//! ブートセクタ(VBR)の解析。
//!
//! exFAT のブート領域はセクタ 0〜11 で、同じものがセクタ 12〜23 に控えている。
//! 先頭が壊れていればバックアップを使う。

use ofr_device::Device;
use ofr_fs::bytes::{u8_at, u32_at, u64_at};
use ofr_fs::{BootSource, FsError, Result};

/// exFAT の識別子。
const FS_NAME: &[u8; 8] = b"EXFAT   ";
/// ブート領域(本体 12 セクタ + バックアップ 12 セクタ)。
const BACKUP_SECTOR: u64 = 12;
/// ブートセクタを総当たりで探す範囲。
const SEARCH_SECTORS: u64 = 64;

/// ブートセクタから読んだ exFAT のジオメトリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExfatBoot {
    /// セクタサイズ。
    pub bytes_per_sector: u32,
    /// 1 クラスタあたりのセクタ数。
    pub sectors_per_cluster: u32,
    /// FAT の開始セクタ。
    pub fat_offset_sectors: u64,
    /// FAT 1 本のセクタ数。
    pub fat_length_sectors: u64,
    /// クラスタヒープの開始セクタ。
    pub heap_offset_sectors: u64,
    /// クラスタ数。
    pub cluster_count: u32,
    /// ルートディレクトリの開始クラスタ。
    pub root_cluster: u32,
    /// FAT の本数。
    pub number_of_fats: u32,
    /// ボリュームのセクタ数。
    pub volume_length: u64,
    /// ボリュームシリアル番号。
    pub volume_serial: Option<u32>,
    /// どこから読めたか。
    pub source: BootSource,
    /// メモ。
    pub notes: Vec<String>,
}

impl ExfatBoot {
    /// ブートセクタを解析する。exFAT として辻褄が合わなければ `None`。
    pub fn parse(sector: &[u8]) -> Option<Self> {
        if sector.get(3..11) != Some(FS_NAME.as_slice()) {
            return None;
        }
        // 11..64 は「必ずゼロ」と決まっている。FAT の BPB と取り違えないための印。
        if sector.get(11..64)?.iter().any(|&b| b != 0) {
            return None;
        }

        let sector_shift = u8_at(sector, 108);
        if !(9..=12).contains(&sector_shift) {
            return None;
        }
        let cluster_shift = u8_at(sector, 109);
        if cluster_shift as u32 + sector_shift as u32 > 25 {
            return None;
        }
        let number_of_fats = u8_at(sector, 110) as u32;
        if !(1..=2).contains(&number_of_fats) {
            return None;
        }

        let bytes_per_sector = 1u32 << sector_shift;
        let sectors_per_cluster = 1u32 << cluster_shift;
        let volume_length = u64_at(sector, 72);
        let fat_offset_sectors = u32_at(sector, 80) as u64;
        let fat_length_sectors = u32_at(sector, 84) as u64;
        let heap_offset_sectors = u32_at(sector, 88) as u64;
        let cluster_count = u32_at(sector, 92);
        let root_cluster = u32_at(sector, 96);

        if fat_offset_sectors < 24 || fat_length_sectors == 0 {
            return None;
        }
        if heap_offset_sectors < fat_offset_sectors + fat_length_sectors * number_of_fats as u64 {
            return None;
        }
        if cluster_count == 0 || cluster_count > 0xFFFF_FFF5 {
            return None;
        }
        if volume_length <= heap_offset_sectors {
            return None;
        }
        if root_cluster < 2 || root_cluster > cluster_count + 1 {
            return None;
        }
        if sector.get(510..512) != Some(&[0x55, 0xAA]) {
            return None;
        }

        Some(Self {
            bytes_per_sector,
            sectors_per_cluster,
            fat_offset_sectors,
            fat_length_sectors,
            heap_offset_sectors,
            cluster_count,
            root_cluster,
            number_of_fats,
            volume_length,
            volume_serial: Some(u32_at(sector, 100)).filter(|v| *v != 0),
            source: BootSource::Primary,
            notes: Vec::new(),
        })
    }

    /// デバイスからブートセクタを探す。
    pub fn probe(device: &dyn Device) -> Result<Self> {
        let mut sector = vec![0u8; 512];

        if device.read_exact_at(0, &mut sector).is_ok()
            && let Some(boot) = Self::parse(&sector)
        {
            return Ok(boot);
        }

        // バックアップはセクタ 12。セクタサイズが 512 でない個体もあるので、
        // ありうるサイズを順に当たる。
        for sector_size in [512u64, 1024, 2048, 4096] {
            let offset = BACKUP_SECTOR * sector_size;
            if device.read_exact_at(offset, &mut sector).is_ok()
                && let Some(mut boot) = Self::parse(&sector)
            {
                boot.source = BootSource::Backup;
                boot.notes.push(format!(
                    "オフセット {offset} のバックアップブートセクタを使った"
                ));
                return Ok(boot);
            }
        }

        for lba in 0..SEARCH_SECTORS {
            if device.read_exact_at(lba * 512, &mut sector).is_ok()
                && let Some(mut boot) = Self::parse(&sector)
            {
                boot.source = BootSource::Backup;
                boot.notes
                    .push(format!("セクタ {lba} で exFAT のブートセクタを見つけた"));
                return Ok(boot);
            }
        }

        Err(FsError::NotRecognized(
            "exFAT のブートセクタが見つからない".to_string(),
        ))
    }

    /// 先頭が exFAT のブートセクタか(素早い判定)。
    pub fn detect(sector: &[u8]) -> bool {
        sector.get(3..11) == Some(FS_NAME.as_slice())
    }

    /// クラスタサイズ(バイト)。
    pub fn cluster_size(&self) -> u32 {
        self.bytes_per_sector * self.sectors_per_cluster
    }

    /// `index` 本目の FAT のオフセット。
    pub fn fat_offset(&self, index: u32) -> u64 {
        (self.fat_offset_sectors + index as u64 * self.fat_length_sectors)
            * self.bytes_per_sector as u64
    }

    /// FAT 1 本のバイト数。
    pub fn fat_bytes(&self) -> u64 {
        self.fat_length_sectors * self.bytes_per_sector as u64
    }

    /// クラスタヒープの開始オフセット。
    pub fn heap_offset(&self) -> u64 {
        self.heap_offset_sectors * self.bytes_per_sector as u64
    }

    /// クラスタ番号のオフセット。
    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.heap_offset() + (cluster.saturating_sub(2)) as u64 * self.cluster_size() as u64
    }

    /// 最後の有効クラスタ番号。
    pub fn last_cluster(&self) -> u32 {
        self.cluster_count.saturating_add(1)
    }

    /// ボリューム全体のバイト数。
    pub fn total_bytes(&self) -> u64 {
        self.volume_length * self.bytes_per_sector as u64
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;
    use ofr_testfs::ExfatImage;

    use super::*;

    #[test]
    fn parses_a_generated_volume() {
        let image = ExfatImage::new(32 << 20).build();
        let boot = ExfatBoot::parse(&image[0..512]).unwrap();
        assert_eq!(boot.bytes_per_sector, 512);
        assert_eq!(boot.cluster_size(), 4096);
        assert_eq!(boot.root_cluster, 2);
        assert_eq!(boot.number_of_fats, 1);
        assert_eq!(boot.source, BootSource::Primary);
        assert_eq!(boot.total_bytes(), 32 << 20);
    }

    #[test]
    fn falls_back_to_the_backup_boot_sector() {
        let mut image = ExfatImage::new(32 << 20).build();
        image[0..512].fill(0);
        let device = MockDevice::builder(image.len() as u64).data(image).build();

        let boot = ExfatBoot::probe(&device).unwrap();
        assert_eq!(boot.source, BootSource::Backup);
        assert_eq!(boot.cluster_size(), 4096);
    }

    #[test]
    fn rejects_nonsense() {
        let image = ExfatImage::new(32 << 20).build();

        let mut broken = image[0..512].to_vec();
        broken[11] = 1; // ゼロでなければならない領域
        assert!(ExfatBoot::parse(&broken).is_none());

        let mut broken = image[0..512].to_vec();
        broken[108] = 20; // ありえないセクタサイズ
        assert!(ExfatBoot::parse(&broken).is_none());

        let mut broken = image[0..512].to_vec();
        broken[96..100].copy_from_slice(&0u32.to_le_bytes()); // ルートクラスタ 0
        assert!(ExfatBoot::parse(&broken).is_none());

        assert!(ExfatBoot::parse(&[0u8; 512]).is_none());
    }

    #[test]
    fn gives_up_when_nothing_is_recognizable() {
        let device = MockDevice::zeroed(4 << 20);
        assert!(matches!(
            ExfatBoot::probe(&device),
            Err(FsError::NotRecognized(_))
        ));
    }
}
