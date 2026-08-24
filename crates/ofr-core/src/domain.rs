//! 吸い出す範囲の組み立て(PLAN.md 11.4)。
//!
//! 死にかけの個体は残り時間が有限で、**どこを先に読むか**が結果を左右する。
//! ここは FS パーサと吸い出しエンジンの間をつなぐ層で、やることは 2 つ。
//!
//! - [`metadata_plan`][]: ブートセクタ、FAT / アロケーションビットマップ、
//!   ルートディレクトリの位置を割り出す。吸い出しはここを最初に確保する
//!   ([`ImageOptions::priority`](ofr_image::ImageOptions::priority))
//! - [`entries_domain`][]: 走査で選んだ項目のデータ位置だけを集める。
//!   ddrescue は `--domain-mapfile` を人手で作らせるが、こちらは FS パーサを
//!   自前で持っているので走査結果から出せる
//!   ([`ImageOptions::domain`](ofr_image::ImageOptions::domain))
//!
//! どちらもオフセットは**デバイス先頭から**に直してある。FS パーサが返すのは
//! ボリューム先頭からの値なので、パーティションの開始位置を足す。

use ofr_device::{Device, SliceDevice};
use ofr_fs::partition::Scheme;
use ofr_fs::{MetaRegion, RecoveredEntry};
use ofr_image::Domain;

use crate::source::{self, Volume};

/// GPT のヘッダとパーティションエントリが収まるセクタ数(LBA 1〜33)。
const GPT_TABLE_SECTORS: u64 = 34;

/// 先に確保しておくメタデータ領域。
#[derive(Debug, Clone, Default)]
pub struct MetadataPlan {
    /// デバイス先頭からの範囲。吸い出しの優先範囲にそのまま渡せる。
    pub domain: Domain,
    /// 内訳(表示用)。ラベルと、デバイス先頭からの位置。
    pub regions: Vec<MetaRegion>,
    /// 見つかったボリューム。判別できなければ `None`。
    pub volume: Option<Volume>,
}

impl MetadataPlan {
    /// 拾えた領域があるか。
    pub fn is_empty(&self) -> bool {
        self.domain.is_empty()
    }
}

/// メタデータの位置を割り出す(PLAN.md 11.4)。
///
/// パーティションテーブルと、見つかったボリュームのメタデータを集める。
/// **失敗しても止めない**: ここは「先に読んでおくと嬉しい範囲」を出すだけなので、
/// ファイルシステムを判別できなければパーティションテーブルの範囲だけを返し、
/// それも読めなければ空を返す。吸い出し自体は普通に続けられる。
pub fn metadata_plan(device: &dyn Device) -> MetadataPlan {
    let mut regions = partition_regions(device);

    let volume = source::locate(device, None, None).ok();
    if let Some(volume) = &volume {
        // ボリューム先頭からの値で返ってくるので、デバイス先頭からに直す。
        if let Ok(region) = SliceDevice::new(device, volume.offset, volume.len)
            && let Ok(fs) = source::open_filesystem(&region, volume.kind)
        {
            regions.extend(fs.metadata_regions().into_iter().map(|mut r| {
                r.extent.offset += volume.offset;
                r
            }));
        }
    }

    MetadataPlan {
        domain: domain_of(&regions),
        regions,
        volume,
    }
}

/// 走査で選んだ項目のデータ位置を集める(PLAN.md 11.4)。
///
/// `volume_offset` はボリュームがデバイスのどこから始まるか。項目の
/// [`extents`](RecoveredEntry::extents) はボリューム先頭からの値なので、これを足す。
pub fn entries_domain<'a>(
    entries: impl IntoIterator<Item = &'a RecoveredEntry>,
    volume_offset: u64,
) -> Domain {
    Domain::new(
        entries
            .into_iter()
            .flat_map(|e| e.extents.iter())
            .map(|x| (x.offset + volume_offset, x.len)),
    )
}

/// パーティションテーブルが置かれている範囲。
///
/// MBR は先頭 1 セクタ、GPT はヘッダとエントリで 34 セクタ、加えて末尾に
/// バックアップがある。ここが読めないとボリュームの位置が分からなくなるので、
/// 何より先に確保する。
fn partition_regions(device: &dyn Device) -> Vec<MetaRegion> {
    let sector = u64::from(device.block_size().max(1));
    let len = device.len();
    let mut out = vec![MetaRegion::new(
        "パーティションテーブル",
        0,
        (GPT_TABLE_SECTORS * sector).min(len),
    )];

    // GPT のバックアップは末尾にある。先頭が壊れたときの命綱。
    let gpt = ofr_fs::partition::list(device)
        .iter()
        .any(|p| p.scheme == Scheme::Gpt);
    if gpt {
        let tail = (GPT_TABLE_SECTORS - 1) * sector;
        if let Some(offset) = len.checked_sub(tail) {
            out.push(MetaRegion::new(
                "バックアップパーティションテーブル",
                offset,
                tail,
            ));
        }
    }
    out
}

/// 領域の並びを範囲の集合にする。
fn domain_of(regions: &[MetaRegion]) -> Domain {
    Domain::new(regions.iter().map(|r| (r.extent.offset, r.extent.len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofr_fs::{EntryKind, EntryStatus, Extent, RecoveredEntry};

    fn entry(extents: &[(u64, u64)]) -> RecoveredEntry {
        let mut e = RecoveredEntry::new("x.jpg", EntryKind::File, EntryStatus::Deleted);
        e.extents = extents
            .iter()
            .map(|&(offset, len)| Extent { offset, len })
            .collect();
        e
    }

    #[test]
    fn shifts_entry_extents_by_the_volume_offset() {
        let a = entry(&[(0, 4096), (8192, 4096)]);
        let b = entry(&[(4096, 2048)]);
        let domain = entries_domain([&a, &b], 1 << 20);

        // 隣り合う範囲はまとまり、間が空いている所は分かれたままになる。
        assert_eq!(
            domain
                .regions()
                .iter()
                .map(|r| (r.pos, r.size))
                .collect::<Vec<_>>(),
            [((1 << 20), 6144), ((1 << 20) + 8192, 4096)]
        );
    }

    #[test]
    fn empty_selection_gives_an_empty_domain() {
        assert!(entries_domain([], 0).is_empty());
    }
}
