//! ディレクトリエントリの解析。
//!
//! exFAT のディレクトリは 32 バイトのエントリを組(エントリセット)にして使う:
//!
//! | エントリ | 型 | 中身 |
//! |---|---|---|
//! | ファイル | `0x85` | 属性、日時、組の個数、組全体のチェックサム |
//! | ストリーム拡張 | `0xC0` | 開始クラスタ、サイズ、`NoFatChain` フラグ |
//! | 名前 | `0xC1` | UTF-16 で 15 文字ずつ |
//!
//! 削除されると各エントリの型バイトから InUse ビット (bit7) が落ちて
//! `0x05` / `0x40` / `0x41` になる。落ちるのはそのビットだけなので、
//! **名前もサイズも開始クラスタも全部残る**。FAT32 の 8.3 名のように
//! 情報が欠けることはない。
//!
//! チェックサムは組全体に対して計算されているので、InUse ビットを立て直して
//! 検算すれば「本物のエントリセットの残骸か、ただのゴミか」を判別できる。
//! これが全クラスタ走査の精度を支えている。

use ofr_fs::bytes::{u8_at, u16_at, u32_at, u64_at, utf16le_string};
use ofr_fs::{Timestamp, Timestamps};

/// 1 エントリのバイト数。
pub const ENTRY_SIZE: usize = 32;

/// InUse ビット。落ちていれば削除済み。
const IN_USE: u8 = 0x80;
/// ファイルエントリ(InUse を除いた型)。
const TYPE_FILE: u8 = 0x05;
/// ストリーム拡張エントリ。
const TYPE_STREAM: u8 = 0x40;
/// ファイル名エントリ。
const TYPE_NAME: u8 = 0x41;
/// アロケーションビットマップ。
const TYPE_BITMAP: u8 = 0x01;
/// ボリュームラベル。
const TYPE_LABEL: u8 = 0x03;

/// ディレクトリ属性。
const ATTR_DIRECTORY: u16 = 0x10;
/// 1 つの組に入る最大エントリ数(ファイル + ストリーム + 名前 17 個)。
const MAX_SECONDARY: u8 = 18;
/// 1 つの名前エントリが持つ文字数。
const NAME_CHARS: usize = 15;

/// ディレクトリエントリ 1 項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExfatEntry {
    /// 名前。
    pub name: String,
    /// 属性(bit4 がディレクトリ)。
    pub attributes: u16,
    /// 開始クラスタ。
    pub first_cluster: u32,
    /// サイズ。
    pub size: u64,
    /// 実際に書かれている長さ。これを超える部分はゼロ。
    pub valid_size: u64,
    /// 削除済みか。
    pub deleted: bool,
    /// FAT チェーンを使わない(= 連続配置が確定している)。
    pub no_fat_chain: bool,
    /// 日時。
    pub times: Timestamps,
    /// 組のチェックサムが合っているか。
    pub checksum_ok: bool,
    /// ディレクトリ先頭からのオフセット。
    pub offset_in_dir: u64,
}

impl ExfatEntry {
    /// ディレクトリか。
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

/// ディレクトリ 1 つ分の解析結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirContents {
    /// 見つかった項目。
    pub entries: Vec<ExfatEntry>,
    /// ボリュームラベル(ルートにだけある)。
    pub volume_label: Option<String>,
    /// アロケーションビットマップの位置(ルートにだけある)。
    pub bitmap: Option<(u32, u64)>,
    /// 終端マーク(型 0x00)に到達したか。
    pub reached_end: bool,
}

/// エントリセットのチェックサム。先頭エントリのチェックサム欄自身は飛ばす。
pub fn set_checksum(entries: &[u8]) -> u16 {
    let mut sum = 0u16;
    for (i, &b) in entries.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(b as u16);
    }
    sum
}

/// 削除済みの組でもチェックサムを検算できるよう、InUse ビットを立て直して計算する。
fn checksum_matches(set: &[u8]) -> bool {
    let expected = u16_at(set, 2);
    if set_checksum(set) == expected {
        return true;
    }
    let mut restored = set.to_vec();
    for chunk in restored.chunks_exact_mut(ENTRY_SIZE) {
        chunk[0] |= IN_USE;
    }
    set_checksum(&restored) == expected
}

/// ディレクトリのバイト列を解析する。
///
/// 終端マークの後ろも走査を続ける。exFAT も FAT32 と同じで、削除された組は
/// そのまま残っているため。
pub fn parse_directory(data: &[u8], base_offset: u64) -> DirContents {
    let mut out = DirContents::default();
    let count = data.len() / ENTRY_SIZE;
    let mut index = 0usize;

    while index < count {
        let at = index * ENTRY_SIZE;
        let raw_type = data[at];
        if raw_type == 0 {
            out.reached_end = true;
            index += 1;
            continue;
        }

        match raw_type & 0x7F {
            TYPE_FILE => {
                let consumed = parse_entry_set(data, index, base_offset, &mut out);
                index += consumed.max(1);
            }
            TYPE_BITMAP if raw_type & IN_USE != 0 => {
                out.bitmap
                    .get_or_insert((u32_at(data, at + 20), u64_at(data, at + 24)));
                index += 1;
            }
            TYPE_LABEL if raw_type & IN_USE != 0 => {
                let chars = (u8_at(data, at + 1) as usize).min(11);
                let units: Vec<u16> = (0..chars).map(|i| u16_at(data, at + 2 + i * 2)).collect();
                let label = utf16le_string(&units);
                if !label.is_empty() {
                    out.volume_label.get_or_insert(label);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    out
}

/// ファイルエントリの組を 1 つ読む。読んだエントリ数を返す。
fn parse_entry_set(data: &[u8], index: usize, base_offset: u64, out: &mut DirContents) -> usize {
    let at = index * ENTRY_SIZE;
    let deleted = data[at] & IN_USE == 0;
    let secondary = u8_at(data, at + 1);
    if !(2..=MAX_SECONDARY).contains(&secondary) {
        return 1;
    }
    let total = 1 + secondary as usize;
    let Some(set) = data.get(at..at + total * ENTRY_SIZE) else {
        return 1;
    };

    // 2 番目はストリーム拡張でなければならない。
    if set[ENTRY_SIZE] & 0x7F != TYPE_STREAM {
        return 1;
    }

    let stream = &set[ENTRY_SIZE..ENTRY_SIZE * 2];
    let flags = u8_at(stream, 1);
    let name_length = u8_at(stream, 3) as usize;

    let mut units = Vec::with_capacity(name_length);
    for i in 2..total {
        let entry = &set[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
        if entry[0] & 0x7F != TYPE_NAME {
            continue;
        }
        for j in 0..NAME_CHARS {
            units.push(u16_at(entry, 2 + j * 2));
        }
    }
    units.truncate(name_length.min(units.len()));
    let name = utf16le_string(&units);
    if name.is_empty() {
        return total;
    }

    out.entries.push(ExfatEntry {
        name,
        attributes: u16_at(set, 4),
        first_cluster: u32_at(stream, 20),
        size: u64_at(stream, 24),
        valid_size: u64_at(stream, 8),
        deleted,
        no_fat_chain: flags & 0x02 != 0,
        times: Timestamps {
            created: timestamp(u32_at(set, 8), u8_at(set, 20), u8_at(set, 22)),
            modified: timestamp(u32_at(set, 12), u8_at(set, 21), u8_at(set, 23)),
            accessed: timestamp(u32_at(set, 16), 0, u8_at(set, 24)),
        },
        checksum_ok: checksum_matches(set),
        offset_in_dir: base_offset + at as u64,
    });
    total
}

/// exFAT のタイムスタンプ(FAT と同じ日付・時刻 + 10ms 補正 + UTC オフセット)。
fn timestamp(packed: u32, fine: u8, utc_offset: u8) -> Option<Timestamp> {
    let date = (packed >> 16) as u16;
    let time = (packed & 0xFFFF) as u16;
    Timestamp::from_fat(date, time, fine.min(199)).map(|t| t.with_exfat_offset(utc_offset))
}

/// このクラスタはディレクトリの先頭か。
///
/// 妥当なエントリセット(チェックサムが合う)で始まっていれば、それは
/// ディレクトリのクラスタとみなしてよい。exFAT には `.` / `..` がないので、
/// 孤立クラスタ走査ではこの判定が目印になる。
pub fn looks_like_directory(cluster: &[u8]) -> bool {
    let Some(head) = cluster.get(0..ENTRY_SIZE) else {
        return false;
    };
    if head[0] & 0x7F != TYPE_FILE {
        return false;
    }
    let secondary = head[1];
    if !(2..=MAX_SECONDARY).contains(&secondary) {
        return false;
    }
    let total = 1 + secondary as usize;
    let Some(set) = cluster.get(0..total * ENTRY_SIZE) else {
        return false;
    };
    if set[ENTRY_SIZE] & 0x7F != TYPE_STREAM {
        return false;
    }
    checksum_matches(set)
}

/// このクラスタでディレクトリが終わっているか。
///
/// 型バイト 0x00 のエントリは「ここから先は未使用」の印。これがあるなら次の
/// クラスタは別のディレクトリなので、連続配置の追跡はここで止める。
pub fn has_end_marker(cluster: &[u8]) -> bool {
    cluster.chunks_exact(ENTRY_SIZE).any(|chunk| chunk[0] == 0)
}

/// ディレクトリの続きのクラスタらしいか(連続配置の追跡用)。
pub fn looks_like_directory_data(cluster: &[u8]) -> bool {
    let mut seen = 0;
    for chunk in cluster.chunks_exact(ENTRY_SIZE).take(8) {
        match chunk[0] {
            0 => break,
            t if matches!(t & 0x7F, TYPE_FILE | TYPE_STREAM | TYPE_NAME) => seen += 1,
            _ => return false,
        }
    }
    seen > 0
}

#[cfg(test)]
mod tests {
    use ofr_testfs::ExfatImage;

    use super::*;

    /// 生成したイメージからルートディレクトリのクラスタを取り出す。
    fn root_cluster(image: &[u8]) -> Vec<u8> {
        let boot = crate::boot::ExfatBoot::parse(&image[0..512]).unwrap();
        let at = boot.cluster_offset(boot.root_cluster) as usize;
        image[at..at + boot.cluster_size() as usize].to_vec()
    }

    fn sample_image() -> Vec<u8> {
        let mut image = ExfatImage::new(32 << 20).label("OFRTEST");
        image.tree().file("/README.TXT", b"hello".to_vec());
        image.tree().file("/長い名前のファイル.txt", vec![7u8; 100]);
        image.tree().file("/GONE.BIN", vec![1u8; 50]);
        image.tree().delete("/GONE.BIN");
        image.build()
    }

    #[test]
    fn reads_a_root_directory() {
        let cluster = root_cluster(&sample_image());
        let dir = parse_directory(&cluster, 0);

        assert_eq!(dir.volume_label.as_deref(), Some("OFRTEST"));
        assert!(dir.bitmap.is_some());
        assert_eq!(dir.entries.len(), 3);

        let readme = &dir.entries[0];
        assert_eq!(readme.name, "README.TXT");
        assert_eq!(readme.size, 5);
        assert!(!readme.deleted);
        assert!(readme.checksum_ok);
        assert!(readme.no_fat_chain, "連続配置なら NoFatChain が立つ");
        assert_eq!(
            readme.times.modified.unwrap().to_string(),
            "2026-08-19 12:34:56"
        );
    }

    #[test]
    fn keeps_the_full_name_of_deleted_entries() {
        let cluster = root_cluster(&sample_image());
        let dir = parse_directory(&cluster, 0);

        let gone = dir.entries.iter().find(|e| e.deleted).unwrap();
        // FAT32 と違い、削除しても名前は 1 文字も欠けない。
        assert_eq!(gone.name, "GONE.BIN");
        assert_eq!(gone.size, 50);
        assert!(gone.checksum_ok, "InUse を戻せばチェックサムは合う");
    }

    #[test]
    fn reads_long_names() {
        let cluster = root_cluster(&sample_image());
        let dir = parse_directory(&cluster, 0);
        let entry = dir
            .entries
            .iter()
            .find(|e| e.name.starts_with("長い"))
            .unwrap();
        assert_eq!(entry.name, "長い名前のファイル.txt");
    }

    #[test]
    fn detects_directory_clusters() {
        let mut image = ExfatImage::new(32 << 20);
        image.tree().file("/DCIM/a.jpg", vec![1u8; 10]);
        let image = image.build();
        let boot = crate::boot::ExfatBoot::parse(&image[0..512]).unwrap();

        // ルートは妥当なエントリセットで始まらない(先頭がビットマップ)ので偽。
        assert!(!looks_like_directory(&root_cluster(&image)));

        // DCIM のクラスタは先頭がファイルエントリセットなので真。
        let dir = parse_directory(&root_cluster(&image), 0);
        let dcim = dir.entries.iter().find(|e| e.is_dir()).unwrap();
        let at = boot.cluster_offset(dcim.first_cluster) as usize;
        let cluster = &image[at..at + boot.cluster_size() as usize];
        assert!(looks_like_directory(cluster));
        assert!(looks_like_directory_data(cluster));
    }

    #[test]
    fn rejects_garbage_as_directories() {
        let garbage = ofr_testfs::pattern_data(3, 4096);
        assert!(!looks_like_directory(&garbage));
        assert!(!looks_like_directory(&[0u8; 4096]));
        assert!(!looks_like_directory(&[0x85, 3]));
    }

    #[test]
    fn never_panics_on_random_bytes() {
        for seed in 0..8 {
            let data = ofr_testfs::pattern_data(seed, 4096);
            let _ = parse_directory(&data, 0);
            let _ = looks_like_directory(&data);
            let _ = parse_directory(&data[..4095], 0);
        }
    }
}
