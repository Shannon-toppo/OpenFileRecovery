//! 復旧元の解決。
//!
//! デバイス ID かイメージファイルを開き、その中のどこにファイルシステムが
//! あるかを割り出す。`ofr image` で取ったイメージはデバイス丸ごとなので、
//! パーティションテーブルを読んでボリュームの開始位置を探す必要がある。

use std::error::Error;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ofr_device::{Device, FileDevice, SliceDevice};
use ofr_exfat::ExfatFs;
use ofr_fat::Fat32Fs;
use ofr_fs::partition::{self, Partition};
use ofr_fs::{FileSystem, FsKind};

/// `--fs` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FsChoice {
    /// 自動判定。
    Auto,
    /// FAT32 として開く。
    Fat32,
    /// exFAT として開く。
    Exfat,
}

impl FsChoice {
    fn kind(self) -> Option<FsKind> {
        match self {
            FsChoice::Auto => None,
            FsChoice::Fat32 => Some(FsKind::Fat32),
            FsChoice::Exfat => Some(FsKind::ExFat),
        }
    }
}

/// 見つかったボリューム。
#[derive(Debug, Clone)]
pub struct Volume {
    /// デバイス先頭からの開始オフセット。
    pub offset: u64,
    /// 長さ。
    pub len: u64,
    /// ファイルシステムの種別。
    pub kind: FsKind,
    /// どのパーティションだったか。
    pub partition: Partition,
}

/// 復旧元を開く。既存のファイルならイメージとして、それ以外はデバイス ID として扱う。
pub fn open_source(source: &str) -> Result<Box<dyn Device>, Box<dyn Error>> {
    let path = Path::new(source);
    if path.is_file() {
        Ok(Box::new(FileDevice::open(path)?))
    } else {
        Ok(ofr_device::open_device(source)?)
    }
}

/// 復旧元として選んでよいデバイスか、列挙情報だけで確かめる(PLAN.md 6章 3項)。
pub fn check_source_selectable(source: &str) -> Result<(), Box<dyn Error>> {
    if Path::new(source).is_file() {
        return Ok(()); // イメージファイルは対象外。
    }
    let Ok(devices) = ofr_device::list_devices() else {
        return Ok(()); // 列挙できない環境では、開いたあとの判定に任せる。
    };
    let Some(info) = devices.iter().find(|d| same_device(&d.id, source)) else {
        return Ok(());
    };
    if info.is_system_disk {
        return Err(format!("{} は起動ディスクなので復旧元にできない", info.id).into());
    }
    Ok(())
}

/// 出力先が復旧元と同じデバイス上にないか確かめる(PLAN.md 6章 2項)。
pub fn check_destination(source_id: &str, dest: &Path) -> Result<(), Box<dyn Error>> {
    let dir = if dest.is_dir() {
        dest.to_path_buf()
    } else {
        dest.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    };
    if let Some(dest_disk) = ofr_device::disk_id_for_path(&dir)
        && same_device(&dest_disk, source_id)
    {
        return Err(format!(
            "出力先 {} は復旧元 {source_id} と同じデバイス上にある。別のディスクを指定すること",
            dest.display()
        )
        .into());
    }
    Ok(())
}

/// デバイス ID の同一判定。`/dev/disk4` と `disk4` のような表記揺れを吸収する。
pub fn same_device(a: &str, b: &str) -> bool {
    fn key(s: &str) -> String {
        s.trim_start_matches(r"\\.\")
            .rsplit('/')
            .next()
            .unwrap_or(s)
            .trim_start_matches('r')
            .to_ascii_lowercase()
    }
    key(a) == key(b)
}

/// ファイルシステムのある位置を探す。
///
/// パーティションテーブルがあればその中を、なければデバイス先頭を候補にする。
/// まず「先頭セクタがブートセクタである」候補を素早く探し、見つからなければ
/// バックアップブートセクタや推定を使う深い判定に降りる。
pub fn locate(
    device: &dyn Device,
    fs: FsChoice,
    offset: Option<u64>,
) -> Result<Volume, Box<dyn Error>> {
    let candidates = match offset {
        Some(offset) => {
            if offset >= device.len() {
                return Err(format!(
                    "オフセット {offset} はデバイスサイズ {} を超えている",
                    device.len()
                )
                .into());
            }
            let mut p = Partition::whole_device(device.len() - offset);
            p.offset = offset;
            p.type_name = "指定オフセット".to_string();
            vec![p]
        }
        None => partition::candidates(device),
    };
    let wanted = fs.kind();

    // 1 周目: 先頭セクタがそのままブートセクタになっている候補。
    for candidate in &candidates {
        let Ok(region) = SliceDevice::new(device, candidate.offset, candidate.len) else {
            continue;
        };
        if wanted != Some(FsKind::Fat32) && ExfatFs::probe(&region) {
            return Ok(volume(candidate, FsKind::ExFat));
        }
        if wanted != Some(FsKind::ExFat) && Fat32Fs::probe(&region) {
            return Ok(volume(candidate, FsKind::Fat32));
        }
    }

    // 2 周目: ブートセクタが壊れている前提で、バックアップと推定まで試す。
    for candidate in &candidates {
        let Ok(region) = SliceDevice::new(device, candidate.offset, candidate.len) else {
            continue;
        };
        if wanted != Some(FsKind::Fat32) && ExfatFs::open(&region).is_ok() {
            return Ok(volume(candidate, FsKind::ExFat));
        }
        if wanted != Some(FsKind::ExFat) && Fat32Fs::open(&region).is_ok() {
            return Ok(volume(candidate, FsKind::Fat32));
        }
    }

    Err(format!(
        "FAT32 / exFAT のボリュームが見つからない(候補 {} 件)。\
         パーティションの位置が分かっているなら --offset で指定する",
        candidates.len()
    )
    .into())
}

fn volume(partition: &Partition, kind: FsKind) -> Volume {
    Volume {
        offset: partition.offset,
        len: partition.len,
        kind,
        partition: partition.clone(),
    }
}

/// 指定された種別でボリュームを開く。
pub fn open_filesystem(
    region: &dyn Device,
    kind: FsKind,
) -> Result<Box<dyn FileSystem + '_>, Box<dyn Error>> {
    Ok(match kind {
        FsKind::Fat32 => Box::new(Fat32Fs::open(region)?),
        FsKind::ExFat => Box::new(ExfatFs::open(region)?),
    })
}

/// Ctrl-C でキャンセルフラグを立てる。
pub fn install_cancel_handler(cancel: Arc<AtomicBool>, message: &'static str) {
    let result = ctrlc::set_handler(move || {
        eprintln!("\n{message}");
        cancel.store(true, Ordering::Relaxed);
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "Ctrl-C ハンドラを登録できなかった");
    }
}

/// `4096` / `64K` / `1M` / `2G` を受け付ける。
pub fn parse_size(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let (digits, mult) = match t.chars().last() {
        Some('K') | Some('k') => (&t[..t.len() - 1], 1024),
        Some('M') | Some('m') => (&t[..t.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("サイズとして読めない: {text}"))?;
    n.checked_mul(mult)
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("サイズが範囲外: {text}"))
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;
    use ofr_testfs::{ExfatImage, Fat32Image};

    use super::*;

    fn mock(image: Vec<u8>) -> MockDevice {
        MockDevice::builder(image.len() as u64).data(image).build()
    }

    #[test]
    fn finds_a_bare_fat32_volume() {
        let device = mock(Fat32Image::new(48 << 20).build());
        let volume = locate(&device, FsChoice::Auto, None).unwrap();
        assert_eq!(volume.kind, FsKind::Fat32);
        assert_eq!(volume.offset, 0);
    }

    #[test]
    fn finds_a_bare_exfat_volume() {
        let device = mock(ExfatImage::new(32 << 20).build());
        let volume = locate(&device, FsChoice::Auto, None).unwrap();
        assert_eq!(volume.kind, FsKind::ExFat);
    }

    /// MBR の後ろにボリュームを置いた、実際の USB メモリに近い構成。
    #[test]
    fn finds_a_volume_behind_a_partition_table() {
        let start = 1 << 20; // 1MiB 目から
        let volume_image = Fat32Image::new(48 << 20).build();
        let mut image = vec![0u8; start as usize + volume_image.len()];
        image[start as usize..].copy_from_slice(&volume_image);

        // MBR: 種別 0x0C (FAT32 LBA) のパーティションを 1 つ。
        let entry = 446;
        image[entry + 4] = 0x0C;
        image[entry + 8..entry + 12].copy_from_slice(&((start / 512) as u32).to_le_bytes());
        image[entry + 12..entry + 16]
            .copy_from_slice(&((volume_image.len() / 512) as u32).to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;

        let device = mock(image);
        let volume = locate(&device, FsChoice::Auto, None).unwrap();
        assert_eq!(volume.kind, FsKind::Fat32);
        assert_eq!(volume.offset, start);
    }

    #[test]
    fn honours_an_explicit_offset() {
        let device = mock(Fat32Image::new(48 << 20).build());
        let err = locate(&device, FsChoice::Auto, Some(1 << 20)).unwrap_err();
        assert!(err.to_string().contains("見つからない"));
    }

    #[test]
    fn refuses_to_open_a_volume_as_the_wrong_type() {
        let device = mock(Fat32Image::new(48 << 20).build());
        assert!(locate(&device, FsChoice::Exfat, None).is_err());
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4096"), Ok(4096));
        assert_eq!(parse_size("1M"), Ok(1 << 20));
        assert!(parse_size("x").is_err());
    }

    #[test]
    fn compares_device_ids_loosely() {
        assert!(same_device("/dev/disk4", "/dev/rdisk4"));
        assert!(same_device(r"\\.\PhysicalDrive2", "PhysicalDrive2"));
        assert!(!same_device("/dev/disk4", "/dev/disk5"));
    }
}
