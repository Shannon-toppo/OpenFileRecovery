//! パーティションテーブルの解析。
//!
//! `ofr image` が取るのはデバイス丸ごとのイメージなので、その中でファイル
//! システムがどこから始まるかを割り出す必要がある。MBR と GPT の両方を読む。
//!
//! パーティションテーブル自体が壊れている場合に備えて、[`candidates`] は
//! 「デバイス先頭をそのままボリュームとみなす」候補も必ず返す。USB メモリや
//! SD カードは、パーティションを切らずに全体を FAT32 にしてある個体も多い。

use ofr_device::Device;

use crate::bytes::{u32_at, u64_at};

/// MBR / GPT の署名位置。
const MBR_SIGNATURE_OFFSET: usize = 510;
const MBR_PARTITION_TABLE_OFFSET: usize = 446;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";

/// パーティションテーブルの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// MBR。
    Mbr,
    /// GPT。
    Gpt,
    /// テーブルなし(デバイス全体が 1 ボリューム)。
    None,
}

impl Scheme {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            Scheme::Mbr => "MBR",
            Scheme::Gpt => "GPT",
            Scheme::None => "パーティションなし",
        }
    }
}

/// 1 つのパーティション(またはデバイス全体)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// テーブル内の番号(1 始まり)。デバイス全体を指す候補は 0。
    pub index: usize,
    /// 開始オフセット(バイト)。
    pub offset: u64,
    /// 長さ(バイト)。
    pub len: u64,
    /// 種類。
    pub scheme: Scheme,
    /// 種別の説明(`FAT32 (0x0C)` など)。
    pub type_name: String,
    /// FAT/exFAT が入っていそうか(種別コードによる目安)。
    pub likely_fat: bool,
}

impl Partition {
    /// デバイス全体を指す候補。
    pub fn whole_device(len: u64) -> Self {
        Self {
            index: 0,
            offset: 0,
            len,
            scheme: Scheme::None,
            type_name: "デバイス全体".to_string(),
            likely_fat: true,
        }
    }
}

/// パーティションテーブルを読む。テーブルがなければ空。
pub fn list(device: &dyn Device) -> Vec<Partition> {
    let sector_size = sector_size(device);
    let mut first = vec![0u8; sector_size as usize];
    if device.read_exact_at(0, &mut first).is_err() {
        return Vec::new();
    }
    if first.get(MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + 2) != Some(&[0x55, 0xAA]) {
        return Vec::new();
    }

    let mbr = parse_mbr(&first, sector_size, device.len());
    // 保護 MBR (種別 0xEE) なら本体は GPT にある。
    if mbr.iter().any(|p| p.type_name.contains("0xEE")) {
        let gpt = parse_gpt(device, sector_size);
        if !gpt.is_empty() {
            return gpt;
        }
    }
    mbr
}

/// 解析を試すべきボリュームの候補を、確からしい順に返す。
///
/// 先頭は必ず「デバイス全体」か、最初の FAT らしいパーティション。
pub fn candidates(device: &dyn Device) -> Vec<Partition> {
    let mut out = Vec::new();
    let partitions = list(device);

    // パーティションテーブルがあっても、先頭セクタが直接ブートセクタである
    // (パーティションを切っていない)可能性は残る。両方候補に入れて、
    // 実際に開けたほうを使う。
    out.push(Partition::whole_device(device.len()));
    for p in partitions {
        if p.len > 0 {
            out.push(p);
        }
    }
    out.sort_by_key(|p| (!p.likely_fat, p.index));
    out
}

fn sector_size(device: &dyn Device) -> u32 {
    match device.block_size() {
        512 | 1024 | 2048 | 4096 => device.block_size(),
        _ => 512,
    }
}

fn parse_mbr(sector: &[u8], sector_size: u32, device_len: u64) -> Vec<Partition> {
    let mut out = Vec::new();
    for i in 0..4 {
        let base = MBR_PARTITION_TABLE_OFFSET + i * 16;
        let Some(entry) = sector.get(base..base + 16) else {
            break;
        };
        let type_code = entry[4];
        let start_lba = u32_at(entry, 8) as u64;
        let sectors = u32_at(entry, 12) as u64;
        if type_code == 0 || sectors == 0 {
            continue;
        }
        let offset = start_lba * sector_size as u64;
        let len = sectors * sector_size as u64;
        if offset >= device_len {
            continue;
        }
        out.push(Partition {
            index: i + 1,
            offset,
            len: len.min(device_len - offset),
            scheme: Scheme::Mbr,
            type_name: format!("{} (0x{type_code:02X})", mbr_type_name(type_code)),
            likely_fat: is_fat_type(type_code),
        });
    }
    out
}

fn parse_gpt(device: &dyn Device, sector_size: u32) -> Vec<Partition> {
    let mut header = vec![0u8; sector_size as usize];
    if device
        .read_exact_at(sector_size as u64, &mut header)
        .is_err()
    {
        return Vec::new();
    }
    if header.get(0..8) != Some(GPT_SIGNATURE) {
        return Vec::new();
    }

    let entry_lba = u64_at(&header, 72);
    let entry_count = u32_at(&header, 80).min(512);
    let entry_size = u32_at(&header, 84);
    if !(128..=1024).contains(&entry_size) {
        return Vec::new();
    }

    let table_bytes = entry_count as u64 * entry_size as u64;
    let mut table = vec![0u8; table_bytes as usize];
    if device
        .read_exact_at(entry_lba * sector_size as u64, &mut table)
        .is_err()
    {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..entry_count as usize {
        let base = i * entry_size as usize;
        let Some(entry) = table.get(base..base + 128) else {
            break;
        };
        if entry[0..16].iter().all(|&b| b == 0) {
            continue; // 未使用エントリ。
        }
        let first_lba = u64_at(entry, 32);
        let last_lba = u64_at(entry, 40);
        if last_lba < first_lba {
            continue;
        }
        let offset = first_lba * sector_size as u64;
        let len = (last_lba - first_lba + 1) * sector_size as u64;
        if offset >= device.len() {
            continue;
        }
        let name = crate::bytes::utf16le_string(
            &entry[56..128]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect::<Vec<_>>(),
        );
        // Microsoft basic data partition。FAT/exFAT はここに入る。
        let basic_data = entry[0..16]
            == [
                0xA2, 0xA0, 0xD0, 0xEB, 0xE5, 0xB9, 0x33, 0x44, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
                0x99, 0xC7,
            ];
        out.push(Partition {
            index: i + 1,
            offset,
            len: len.min(device.len() - offset),
            scheme: Scheme::Gpt,
            type_name: if name.is_empty() {
                "GPT パーティション".to_string()
            } else {
                name
            },
            likely_fat: basic_data,
        });
    }
    out
}

fn is_fat_type(code: u8) -> bool {
    matches!(
        code,
        0x01 | 0x04 | 0x06 | 0x07 | 0x0B | 0x0C | 0x0E | 0x1B | 0x1C | 0x1E
    )
}

fn mbr_type_name(code: u8) -> &'static str {
    match code {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0E => "FAT16",
        0x07 => "exFAT / NTFS",
        0x0B | 0x0C => "FAT32",
        0x0F | 0x05 => "拡張パーティション",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0xAF => "HFS+",
        0xEE => "GPT 保護",
        0xEF => "EFI システム",
        _ => "不明",
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;

    /// 8MiB のデバイスに MBR だけを書いたモック。
    fn mbr_device(entries: &[(u8, u32, u32)]) -> MockDevice {
        let mut data = vec![0u8; 8 << 20];
        for (i, (kind, start, sectors)) in entries.iter().enumerate() {
            let base = MBR_PARTITION_TABLE_OFFSET + i * 16;
            data[base + 4] = *kind;
            data[base + 8..base + 12].copy_from_slice(&start.to_le_bytes());
            data[base + 12..base + 16].copy_from_slice(&sectors.to_le_bytes());
        }
        data[MBR_SIGNATURE_OFFSET] = 0x55;
        data[MBR_SIGNATURE_OFFSET + 1] = 0xAA;
        MockDevice::builder(8 << 20).data(data).build()
    }

    #[test]
    fn reads_mbr_entries() {
        let device = mbr_device(&[(0x0C, 2048, 1024), (0x83, 4096, 512)]);
        let parts = list(&device);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].offset, 2048 * 512);
        assert_eq!(parts[0].len, 1024 * 512);
        assert!(parts[0].likely_fat);
        assert!(parts[0].type_name.starts_with("FAT32"));
        assert!(!parts[1].likely_fat);
    }

    #[test]
    fn ignores_devices_without_a_signature() {
        let device = MockDevice::zeroed(8 << 20);
        assert!(list(&device).is_empty());
        // それでも「デバイス全体」の候補は返る。
        let candidates = candidates(&device);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].offset, 0);
    }

    #[test]
    fn puts_fat_partitions_before_others() {
        let device = mbr_device(&[(0x83, 2048, 1024), (0x0C, 4096, 1024)]);
        let candidates = candidates(&device);
        assert_eq!(candidates[0].index, 0); // デバイス全体
        assert_eq!(candidates[1].offset, 4096 * 512); // FAT32
        assert_eq!(candidates[2].offset, 2048 * 512); // Linux
    }

    #[test]
    fn clamps_partitions_to_the_device() {
        let device = mbr_device(&[(0x0C, 2000, 100_000)]);
        let parts = list(&device);
        assert_eq!(parts[0].offset + parts[0].len, 8 << 20);
    }
}
