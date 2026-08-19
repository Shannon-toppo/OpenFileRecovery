//! ブートセクタ(BPB)の解析と、壊れていた場合のジオメトリ推定。
//!
//! FAT32 のブートセクタはセクタ 0 にあり、同じものがセクタ 6(BPB の
//! `BkBootSec` が指す位置)にコピーされている。両方壊れている個体もあるので、
//! その場合は FAT 表そのものを探してジオメトリを逆算する。

use ofr_device::Device;
use ofr_fs::bytes::{u8_at, u16_at, u32_at};
use ofr_fs::{BootSource, FsError, Result};

/// ブートセクタから読んだ FAT32 のジオメトリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fat32Bpb {
    /// セクタサイズ。
    pub bytes_per_sector: u32,
    /// 1 クラスタあたりのセクタ数。
    pub sectors_per_cluster: u32,
    /// 予約セクタ数(= 最初の FAT までのセクタ数)。
    pub reserved_sectors: u32,
    /// FAT の本数。
    pub num_fats: u32,
    /// ボリューム全体のセクタ数。
    pub total_sectors: u64,
    /// FAT 1 本分のセクタ数。
    pub fat_size_sectors: u64,
    /// ルートディレクトリの開始クラスタ。
    pub root_cluster: u32,
    /// ボリュームラベル(BPB 側。ルートの volume-id エントリのほうが新しい)。
    pub volume_label: Option<String>,
    /// ボリュームシリアル番号。
    pub volume_serial: Option<u32>,
    /// どこから読めたか。
    pub source: BootSource,
    /// 推定で埋めた項目などのメモ。
    pub notes: Vec<String>,
}

/// FAT 表の先頭にある予約エントリ。メディア記述子 0xF8 + 終端マーク。
const FAT_SIGNATURE: [u8; 7] = [0xF8, 0xFF, 0xFF, 0x0F, 0xFF, 0xFF, 0xFF];
/// 推定時に FAT 表を探す範囲。
const FAT_SEARCH_BYTES: u64 = 8 << 20;
/// バックアップブートセクタを探す範囲(セクタ数)。
const BOOT_SEARCH_SECTORS: u64 = 64;

impl Fat32Bpb {
    /// 512 バイトのブートセクタを解析する。FAT32 として辻褄が合わなければ `None`。
    pub fn parse(sector: &[u8]) -> Option<Self> {
        let bytes_per_sector = u16_at(sector, 11) as u32;
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return None;
        }
        let sectors_per_cluster = u8_at(sector, 13) as u32;
        if sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster > 128
        {
            return None;
        }
        let reserved_sectors = u16_at(sector, 14) as u32;
        if reserved_sectors == 0 {
            return None;
        }
        let num_fats = u8_at(sector, 16) as u32;
        if !(1..=4).contains(&num_fats) {
            return None;
        }
        // FAT32 ではルートディレクトリエントリ数と 16bit の FAT サイズは 0。
        if u16_at(sector, 17) != 0 || u16_at(sector, 22) != 0 {
            return None;
        }
        let total_sectors = match (u16_at(sector, 19) as u64, u32_at(sector, 32) as u64) {
            (0, t32) if t32 > 0 => t32,
            _ => return None,
        };
        let fat_size_sectors = u32_at(sector, 36) as u64;
        if fat_size_sectors == 0 {
            return None;
        }
        let root_cluster = u32_at(sector, 44);
        if root_cluster < 2 {
            return None;
        }

        // ブートセクタらしさの確認。どちらか一方でも通れば採用する
        // (壊れかけメディアでは片方だけ化けていることがある)。
        let has_signature = sector.get(510..512) == Some(&[0x55, 0xAA]);
        let fs_type = sector.get(82..90).unwrap_or(&[]);
        if !has_signature && fs_type != b"FAT32   " {
            return None;
        }

        // データ領域が残っているか。
        let meta_sectors = reserved_sectors as u64 + num_fats as u64 * fat_size_sectors;
        if meta_sectors >= total_sectors {
            return None;
        }

        Some(Self {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            total_sectors,
            fat_size_sectors,
            root_cluster,
            volume_label: read_label(sector),
            volume_serial: Some(u32_at(sector, 67)).filter(|v| *v != 0),
            source: BootSource::Primary,
            notes: Vec::new(),
        })
    }

    /// デバイスからブートセクタを探す。
    ///
    /// セクタ 0 →(BPB が指す)バックアップ → 先頭付近の総当たり →
    /// FAT 表からの推定、の順に試す。
    pub fn probe(device: &dyn Device) -> Result<Self> {
        let mut sector = vec![0u8; 512];

        if device.read_exact_at(0, &mut sector).is_ok()
            && let Some(bpb) = Self::parse(&sector)
        {
            return Ok(bpb);
        }

        // バックアップの位置は普通セクタ 6。壊れた BPB からは位置を読めないので、
        // 一般的な値と、その周辺を順に当たる。
        for lba in [6u64, 12, 1, 2, 3, 4, 5, 7, 8] {
            if device.read_exact_at(lba * 512, &mut sector).is_ok()
                && let Some(mut bpb) = Self::parse(&sector)
            {
                bpb.source = BootSource::Backup;
                bpb.notes
                    .push(format!("セクタ {lba} のバックアップブートセクタを使った"));
                return Ok(bpb);
            }
        }

        // 先頭付近を総当たり(セクタサイズが 512 でない個体への保険)。
        for lba in 0..BOOT_SEARCH_SECTORS {
            if device.read_exact_at(lba * 512, &mut sector).is_ok()
                && let Some(mut bpb) = Self::parse(&sector)
            {
                bpb.source = BootSource::Backup;
                bpb.notes
                    .push(format!("セクタ {lba} で FAT32 のブートセクタを見つけた"));
                return Ok(bpb);
            }
        }

        Self::estimate(device)
    }

    /// ブートセクタが全滅している場合に、FAT 表の位置からジオメトリを推定する。
    ///
    /// FAT32 の FAT 表は必ず `F8 FF FF 0F FF FF FF ..` で始まる。この並びが
    /// セクタ境界に現れる位置を探し、2 本目との距離から FAT のサイズを、
    /// データ領域の広さとの比からクラスタサイズを割り出す。
    fn estimate(device: &dyn Device) -> Result<Self> {
        let bytes_per_sector = 512u64;
        let search_len = FAT_SEARCH_BYTES.min(device.len());
        let mut found = Vec::new();

        let chunk_size = 1 << 20;
        let mut buf = vec![0u8; chunk_size];
        let mut pos = 0u64;
        while pos < search_len && found.len() < 2 {
            let want = chunk_size.min((search_len - pos) as usize);
            if device.read_exact_at(pos, &mut buf[..want]).is_err() {
                pos += want as u64;
                continue;
            }
            for offset in (0..want).step_by(bytes_per_sector as usize) {
                if buf[offset..].starts_with(&FAT_SIGNATURE) {
                    found.push(pos + offset as u64);
                    if found.len() >= 2 {
                        break;
                    }
                }
            }
            pos += want as u64;
        }

        let Some(&first_fat) = found.first() else {
            return Err(FsError::NotRecognized(
                "ブートセクタも FAT 表も見つからない".to_string(),
            ));
        };

        let reserved_sectors = (first_fat / bytes_per_sector) as u32;
        let (num_fats, fat_size_sectors) = match found.get(1) {
            Some(&second) if second > first_fat => (2u32, (second - first_fat) / bytes_per_sector),
            // 2 本目が見つからないなら 1 本構成とみなす。サイズはデバイス全体から逆算する。
            _ => (1u32, estimate_fat_size(device.len(), bytes_per_sector)),
        };

        let total_sectors = device.len() / bytes_per_sector;
        let meta_sectors = reserved_sectors as u64 + num_fats as u64 * fat_size_sectors;
        if meta_sectors >= total_sectors {
            return Err(FsError::NotRecognized(
                "FAT 表は見つかったがジオメトリを推定できない".to_string(),
            ));
        }

        // FAT 表 1 本が扱えるクラスタ数から、1 クラスタのセクタ数を逆算する。
        let data_sectors = total_sectors - meta_sectors;
        let max_clusters = (fat_size_sectors * bytes_per_sector / 4).max(1);
        // 端数を切り上げる。切り捨てるとデータ領域の後ろが FAT の届かない
        // 範囲になってしまう。
        let sectors_per_cluster = data_sectors.div_ceil(max_clusters).max(1);
        let sectors_per_cluster = sectors_per_cluster.next_power_of_two().clamp(1, 128) as u32;

        Ok(Self {
            bytes_per_sector: bytes_per_sector as u32,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            total_sectors,
            fat_size_sectors,
            root_cluster: 2,
            volume_label: None,
            volume_serial: None,
            source: BootSource::Estimated,
            notes: vec![format!(
                "ブートセクタが両方とも壊れていたので、FAT 表(オフセット {first_fat})から\
                 予約 {reserved_sectors} セクタ / FAT {fat_size_sectors} セクタ × {num_fats} / \
                 クラスタ {sectors_per_cluster} セクタと推定した。\
                 ルートは既定のクラスタ 2 と仮定している"
            )],
        })
    }

    /// クラスタサイズ(バイト)。
    pub fn cluster_size(&self) -> u32 {
        self.bytes_per_sector * self.sectors_per_cluster
    }

    /// `index` 本目の FAT のオフセット(バイト)。
    pub fn fat_offset(&self, index: u32) -> u64 {
        (self.reserved_sectors as u64 + index as u64 * self.fat_size_sectors)
            * self.bytes_per_sector as u64
    }

    /// FAT 1 本のバイト数。
    pub fn fat_bytes(&self) -> u64 {
        self.fat_size_sectors * self.bytes_per_sector as u64
    }

    /// データ領域の開始オフセット(バイト)。
    pub fn data_offset(&self) -> u64 {
        (self.reserved_sectors as u64 + self.num_fats as u64 * self.fat_size_sectors)
            * self.bytes_per_sector as u64
    }

    /// データ領域のクラスタ数。
    pub fn cluster_count(&self) -> u32 {
        let meta = self.reserved_sectors as u64 + self.num_fats as u64 * self.fat_size_sectors;
        let data_sectors = self.total_sectors.saturating_sub(meta);
        (data_sectors / self.sectors_per_cluster as u64).min(u32::MAX as u64 - 2) as u32
    }

    /// 最後の有効クラスタ番号。
    pub fn last_cluster(&self) -> u32 {
        self.cluster_count().saturating_add(1)
    }

    /// クラスタ番号のオフセット(バイト)。
    pub fn cluster_offset(&self, cluster: u32) -> u64 {
        self.data_offset() + (cluster.saturating_sub(2)) as u64 * self.cluster_size() as u64
    }

    /// ボリューム全体のバイト数。
    pub fn total_bytes(&self) -> u64 {
        self.total_sectors * self.bytes_per_sector as u64
    }
}

fn estimate_fat_size(device_len: u64, bytes_per_sector: u64) -> u64 {
    // クラスタサイズが分からない段階の当て推量。32KiB クラスタを仮定する
    // (16GiB 超の FAT32 でよくある構成)。
    let clusters = device_len / (32 << 10);
    (clusters * 4).div_ceil(bytes_per_sector).max(1)
}

fn read_label(sector: &[u8]) -> Option<String> {
    let raw = sector.get(71..82)?;
    let label = ofr_fs::bytes::oem_string(raw).trim_end().to_string();
    (!label.is_empty() && label != "NO NAME").then_some(label)
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;

    /// 最小限の FAT32 ブートセクタを組み立てる。
    fn boot_sector(sectors_per_cluster: u8, fat_size: u32, total: u32) -> Vec<u8> {
        let mut s = vec![0u8; 512];
        s[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        s[3..11].copy_from_slice(b"MSDOS5.0");
        s[11..13].copy_from_slice(&512u16.to_le_bytes());
        s[13] = sectors_per_cluster;
        s[14..16].copy_from_slice(&32u16.to_le_bytes());
        s[16] = 2;
        s[21] = 0xF8;
        s[32..36].copy_from_slice(&total.to_le_bytes());
        s[36..40].copy_from_slice(&fat_size.to_le_bytes());
        s[44..48].copy_from_slice(&2u32.to_le_bytes());
        s[67..71].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        s[71..82].copy_from_slice(b"OFRTEST    ");
        s[82..90].copy_from_slice(b"FAT32   ");
        s[510] = 0x55;
        s[511] = 0xAA;
        s
    }

    #[test]
    fn parses_a_normal_boot_sector() {
        let bpb = Fat32Bpb::parse(&boot_sector(8, 128, 16384)).unwrap();
        assert_eq!(bpb.bytes_per_sector, 512);
        assert_eq!(bpb.cluster_size(), 4096);
        assert_eq!(bpb.data_offset(), (32 + 256) * 512);
        assert_eq!(bpb.cluster_offset(2), bpb.data_offset());
        assert_eq!(bpb.volume_label.as_deref(), Some("OFRTEST"));
        assert_eq!(bpb.source, BootSource::Primary);
    }

    #[test]
    fn rejects_nonsense() {
        let mut s = boot_sector(8, 128, 16384);
        s[13] = 3; // 2 の冪でないクラスタサイズ
        assert!(Fat32Bpb::parse(&s).is_none());

        let mut s = boot_sector(8, 128, 16384);
        s[17] = 1; // FAT32 ではルートエントリ数は 0
        assert!(Fat32Bpb::parse(&s).is_none());

        let mut s = boot_sector(8, 128, 16384);
        s[32..36].copy_from_slice(&100u32.to_le_bytes()); // メタ領域より小さい総セクタ数
        assert!(Fat32Bpb::parse(&s).is_none());

        assert!(Fat32Bpb::parse(&[0u8; 512]).is_none());
    }

    #[test]
    fn falls_back_to_the_backup_boot_sector() {
        let mut data = vec![0u8; 16 << 20];
        data[6 * 512..7 * 512].copy_from_slice(&boot_sector(8, 128, 16384));
        let device = MockDevice::builder(16 << 20).data(data).build();

        let bpb = Fat32Bpb::probe(&device).unwrap();
        assert_eq!(bpb.source, BootSource::Backup);
        assert_eq!(bpb.cluster_size(), 4096);
    }

    #[test]
    fn estimates_geometry_from_the_fat_tables() {
        // ブートセクタは全滅。FAT が 32 セクタ目と 160 セクタ目にある構成。
        let mut data = vec![0u8; 16 << 20];
        for lba in [32usize, 160] {
            data[lba * 512..lba * 512 + 7].copy_from_slice(&FAT_SIGNATURE);
        }
        let device = MockDevice::builder(16 << 20).data(data).build();

        let bpb = Fat32Bpb::probe(&device).unwrap();
        assert_eq!(bpb.source, BootSource::Estimated);
        assert_eq!(bpb.reserved_sectors, 32);
        assert_eq!(bpb.num_fats, 2);
        assert_eq!(bpb.fat_size_sectors, 128);
        assert_eq!(bpb.root_cluster, 2);
        // FAT 128 セクタ = 16384 項目。データ 32480 セクタ ÷ 16384 → 2 セクタ/クラスタ。
        assert_eq!(bpb.sectors_per_cluster, 2);
        assert!(!bpb.notes.is_empty());
    }

    #[test]
    fn gives_up_when_nothing_is_recognizable() {
        let device = MockDevice::zeroed(4 << 20);
        assert!(matches!(
            Fat32Bpb::probe(&device),
            Err(FsError::NotRecognized(_))
        ));
    }
}
