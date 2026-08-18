//! ディレクトリエントリ(32 バイト)の解析。
//!
//! 削除済みエントリの扱いがこのモジュールの本題:
//!
//! - 削除されると、そのファイルの**全エントリ**(LFN も短い名前も)の先頭バイトが
//!   `0xE5` になる。LFN の並び番号もこのバイトなので失われるが、LFN は物理的に
//!   逆順(最後の断片が先)に置かれる決まりなので、並びからは復元できる
//! - 短い名前は先頭 1 文字が失われる。ただし LFN が残っていれば、その checksum
//!   (11 バイトの短い名前から計算する)と突き合わせて元の 1 文字を割り出せる
//! - LFN が無ければ名前は `_AMPLE.TXT` のように先頭が欠けたままになる

use ofr_fs::bytes::{oem_string, u8_at, u16_at, u32_at, utf16le_string};
use ofr_fs::{Timestamp, Timestamps};

/// 1 エントリのバイト数。
pub const ENTRY_SIZE: usize = 32;

/// 削除マーク。
const DELETED_MARK: u8 = 0xE5;
/// 空きエントリ(以降は未使用)。
const END_MARK: u8 = 0x00;

/// 読み取り専用属性。
pub const ATTR_READ_ONLY: u8 = 0x01;
/// 隠し属性。
pub const ATTR_HIDDEN: u8 = 0x02;
/// システム属性。
pub const ATTR_SYSTEM: u8 = 0x04;
/// ボリュームラベル。
pub const ATTR_VOLUME_ID: u8 = 0x08;
/// ディレクトリ。
pub const ATTR_DIRECTORY: u8 = 0x10;
/// 長いファイル名(LFN)エントリ。
pub const ATTR_LFN: u8 = 0x0F;

/// 1 つのディレクトリエントリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// 表示に使う名前(LFN があればそれ)。
    pub name: String,
    /// 8.3 形式の名前。
    pub short_name: String,
    /// 属性バイト。
    pub attributes: u8,
    /// 開始クラスタ。
    pub first_cluster: u32,
    /// ファイルサイズ。
    pub size: u32,
    /// 削除済みか。
    pub deleted: bool,
    /// 日時。
    pub times: Timestamps,
    /// 名前を完全には復元できていない(先頭 1 文字が不明)。
    pub name_partial: bool,
    /// ディレクトリ先頭からのバイトオフセット。
    pub offset_in_dir: u64,
}

impl DirEntry {
    /// ディレクトリか。
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

/// ディレクトリ 1 つ分の解析結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirContents {
    /// 見つかった項目。
    pub entries: Vec<DirEntry>,
    /// ボリュームラベル(ルートディレクトリにだけ入っている)。
    pub volume_label: Option<String>,
    /// 終端マーク(先頭バイト 0x00)に到達したか。
    pub reached_end: bool,
}

/// 8.3 名の checksum。LFN エントリが持っている値と突き合わせる。
pub fn short_name_checksum(raw: &[u8]) -> u8 {
    let mut sum = 0u8;
    for i in 0..11 {
        sum = sum.rotate_right(1).wrapping_add(u8_at(raw, i));
    }
    sum
}

/// ディレクトリのバイト列を解析する。
///
/// 終端マークの後ろも走査を続ける。削除されたエントリはそこに残っていることが
/// あるため(ディレクトリを縮める処理は行われない)。
pub fn parse_directory(data: &[u8], base_offset: u64) -> DirContents {
    let mut out = DirContents::default();
    // LFN は「最後の断片が先」の物理順で並ぶ。順番のまま溜めて、短い名前の
    // エントリに出会った時点で逆順に連結する。
    let mut lfn: Vec<LfnPart> = Vec::new();

    for (index, chunk) in data.chunks_exact(ENTRY_SIZE).enumerate() {
        let offset_in_dir = base_offset + (index * ENTRY_SIZE) as u64;
        let first = chunk[0];

        if first == END_MARK {
            out.reached_end = true;
            lfn.clear();
            continue;
        }

        let attributes = u8_at(chunk, 11);
        let deleted = first == DELETED_MARK;

        if attributes & 0x3F == ATTR_LFN {
            lfn.push(LfnPart {
                sequence: first & 0x1F,
                last: first & 0x40 != 0,
                checksum: u8_at(chunk, 13),
                chars: lfn_chars(chunk),
            });
            continue;
        }

        if attributes & ATTR_VOLUME_ID != 0 && attributes & ATTR_DIRECTORY == 0 {
            if !deleted && out.volume_label.is_none() {
                let label = oem_string(&chunk[0..11]).trim_end().to_string();
                if !label.is_empty() {
                    out.volume_label = Some(label);
                }
            }
            lfn.clear();
            continue;
        }

        // "." と ".." は自分自身と親を指すだけなので項目にはしない。
        if chunk[0] == b'.' || (deleted && chunk[1] == b'.' && chunk[2] == b' ') {
            lfn.clear();
            continue;
        }

        // 属性バイトが化けているエントリは信用しない。
        if attributes & 0xC0 != 0 {
            lfn.clear();
            continue;
        }

        let (name, short_name, name_partial) = build_name(chunk, &lfn, deleted);
        lfn.clear();

        if name.is_empty() {
            continue;
        }

        let first_cluster = ((u16_at(chunk, 20) as u32) << 16) | u16_at(chunk, 26) as u32;
        out.entries.push(DirEntry {
            name,
            short_name,
            attributes,
            first_cluster,
            size: u32_at(chunk, 28),
            deleted,
            times: Timestamps {
                created: Timestamp::from_fat(
                    u16_at(chunk, 16),
                    u16_at(chunk, 14),
                    u8_at(chunk, 13),
                ),
                modified: Timestamp::from_fat(u16_at(chunk, 24), u16_at(chunk, 22), 0),
                accessed: Timestamp::from_fat(u16_at(chunk, 18), 0, 0),
            },
            name_partial,
            offset_in_dir,
        });
    }

    out
}

/// このクラスタはディレクトリの先頭か。
///
/// サブディレクトリのクラスタは必ず `.` と `..` のエントリで始まる。これは
/// ルートから辿れなくなったディレクトリを全クラスタ走査で拾うときの目印になる
/// (PLAN.md 5.3)。削除されたディレクトリでも、この 2 つのエントリ自体は
/// 書き換えられないので残っている。
pub fn looks_like_directory(cluster: &[u8]) -> bool {
    let Some(dot) = cluster.get(0..ENTRY_SIZE) else {
        return false;
    };
    let Some(dotdot) = cluster.get(ENTRY_SIZE..ENTRY_SIZE * 2) else {
        return false;
    };

    let dot_name = &dot[0..11];
    let dotdot_name = &dotdot[0..11];
    let dot_ok = (dot_name[0] == b'.' || dot_name[0] == DELETED_MARK)
        && dot_name[1..] == *b"          "
        && dot[11] & ATTR_DIRECTORY != 0;
    let dotdot_ok = (dotdot_name[0] == b'.' || dotdot_name[0] == DELETED_MARK)
        && dotdot_name[1] == b'.'
        && dotdot_name[2..] == *b"         "
        && dotdot[11] & ATTR_DIRECTORY != 0;

    dot_ok && dotdot_ok
}

/// このクラスタでディレクトリが終わっているか。
///
/// 先頭バイト 0x00 のエントリは「ここから先は未使用」の印。これがあるなら
/// 次のクラスタは別のディレクトリなので、連続配置の追跡はここで止める。
pub fn has_end_marker(cluster: &[u8]) -> bool {
    cluster
        .chunks_exact(ENTRY_SIZE)
        .any(|chunk| chunk[0] == END_MARK)
}

/// ディレクトリの続きのクラスタらしいか(連続配置の追跡用)。
///
/// 先頭が使用中のエントリで、属性バイトが妥当なら続きとみなす。
pub fn looks_like_directory_data(cluster: &[u8]) -> bool {
    let mut seen = 0;
    for chunk in cluster.chunks_exact(ENTRY_SIZE).take(8) {
        if chunk[0] == END_MARK {
            break;
        }
        let attributes = u8_at(chunk, 11);
        if attributes & 0xC0 != 0 || attributes == 0 {
            return false;
        }
        seen += 1;
    }
    seen > 0
}

struct LfnPart {
    sequence: u8,
    last: bool,
    checksum: u8,
    chars: [u16; 13],
}

fn lfn_chars(chunk: &[u8]) -> [u16; 13] {
    let mut chars = [0u16; 13];
    let positions = [
        1, 3, 5, 7, 9, // 5 文字
        14, 16, 18, 20, 22, 24, // 6 文字
        28, 30, // 2 文字
    ];
    for (i, &pos) in positions.iter().enumerate() {
        chars[i] = u16_at(chunk, pos);
    }
    chars
}

/// 表示名・8.3 名・「名前が欠けているか」を組み立てる。
fn build_name(chunk: &[u8], lfn: &[LfnPart], deleted: bool) -> (String, String, bool) {
    let raw = &chunk[0..11];
    let checksum_target = if deleted {
        // 先頭バイトが 0xE5 に潰されているので、LFN の checksum と一致する
        // 元の 1 文字を総当たりで割り出す。
        lfn.last()
            .and_then(|part| recover_first_byte(raw, part.checksum))
    } else {
        None
    };

    let mut restored = [0u8; 11];
    restored.copy_from_slice(raw);
    if let Some(byte) = checksum_target {
        restored[0] = byte;
    }

    let long = assemble_lfn(lfn, &restored, deleted);
    let short = format_short_name(&restored, u8_at(chunk, 12));

    match long {
        // LFN には元の名前がそのまま残っているので、削除済みでも欠けはない。
        Some(name) => (name, short, false),
        None if deleted && checksum_target.is_none() => {
            // 先頭 1 文字が分からないままの 8.3 名。
            let mut name = short.clone();
            if !name.is_empty() {
                name.replace_range(0..1, "_");
            }
            (name, short, true)
        }
        None => (short.clone(), short, false),
    }
}

/// 溜めた LFN 断片から名前を組み立てる。
///
/// 削除済みの場合は並び番号が失われているので、物理順(逆順に並んでいる)を
/// そのまま信じる。生きている場合は番号と checksum で検証し、合わなければ
/// 「別のエントリの残骸」とみなして捨てる。
fn assemble_lfn(lfn: &[LfnPart], short_name: &[u8; 11], deleted: bool) -> Option<String> {
    if lfn.is_empty() {
        return None;
    }
    let checksum = short_name_checksum(short_name);
    let checksum_ok = lfn.iter().all(|p| p.checksum == lfn[0].checksum)
        && (deleted || lfn.iter().all(|p| p.checksum == checksum));
    if !checksum_ok {
        return None;
    }
    if !deleted {
        // 生きているエントリは並び番号も検証する(N, N-1, ..., 1 の順)。
        if !lfn[0].last || lfn[0].sequence as usize != lfn.len() {
            return None;
        }
        for (i, part) in lfn.iter().enumerate() {
            if part.sequence as usize != lfn.len() - i {
                return None;
            }
        }
    }

    let mut units = Vec::with_capacity(lfn.len() * 13);
    for part in lfn.iter().rev() {
        units.extend_from_slice(&part.chars);
    }
    // 末尾の埋め草 (0xFFFF) を落としてから終端 0 で切る。
    while units.last() == Some(&0xFFFF) {
        units.pop();
    }
    let name = utf16le_string(&units);
    (!name.is_empty()).then_some(name)
}

/// 削除で潰された 8.3 名の先頭 1 バイトを、LFN の checksum から復元する。
fn recover_first_byte(raw: &[u8], checksum: u8) -> Option<u8> {
    let mut candidate = [0u8; 11];
    candidate.copy_from_slice(&raw[0..11]);
    // ありそうな文字から順に試す。checksum は 8bit なので複数当たることがあるが、
    // 先に見つかった「ファイル名に使われやすい文字」を採る。
    let order = (b'A'..=b'Z')
        .chain(b'0'..=b'9')
        .chain([b'_', b'-', b'~', b'!', b'#', b'$', b'%', b'&', b'@', b'^'])
        .chain(0x80..=0xFF)
        .chain(b'a'..=b'z');
    for byte in order {
        candidate[0] = byte;
        if short_name_checksum(&candidate) == checksum {
            return Some(byte);
        }
    }
    None
}

/// 8.3 名を表示用の文字列にする。
fn format_short_name(raw: &[u8; 11], case_flags: u8) -> String {
    let mut name = raw;
    let mut fixed = *raw;
    // 0x05 は「先頭バイトが本当は 0xE5」の意味(日本語の先頭バイト対策)。
    if fixed[0] == 0x05 {
        fixed[0] = 0xE5;
        name = &fixed;
    }

    let base_raw = oem_string(&name[0..8]);
    let ext_raw = oem_string(&name[8..11]);
    let mut base = base_raw.trim_end().to_string();
    let mut ext = ext_raw.trim_end().to_string();

    // Windows が付ける「本当は小文字」フラグ。
    if case_flags & 0x08 != 0 {
        base = base.to_lowercase();
    }
    if case_flags & 0x10 != 0 {
        ext = ext.to_lowercase();
    }

    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short_entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32) -> Vec<u8> {
        let mut e = vec![0u8; ENTRY_SIZE];
        e[0..11].copy_from_slice(name);
        e[11] = attr;
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        // 2026-08-19 12:00:00
        let date = ((2026u16 - 1980) << 9) | (8 << 5) | 19;
        e[24..26].copy_from_slice(&date.to_le_bytes());
        e[22..24].copy_from_slice(&(12u16 << 11).to_le_bytes());
        e
    }

    fn lfn_entry(seq: u8, last: bool, checksum: u8, chars: &[u16]) -> Vec<u8> {
        let mut e = vec![0u8; ENTRY_SIZE];
        e[0] = seq | if last { 0x40 } else { 0 };
        e[11] = ATTR_LFN;
        e[13] = checksum;
        let positions = [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (i, &pos) in positions.iter().enumerate() {
            let value = chars.get(i).copied().unwrap_or(0xFFFF);
            e[pos..pos + 2].copy_from_slice(&value.to_le_bytes());
        }
        e
    }

    fn utf16(s: &str) -> Vec<u16> {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        v
    }

    #[test]
    fn parses_a_short_name_entry() {
        let data = short_entry(b"REPORT  TXT", 0x20, 5, 1234);
        let dir = parse_directory(&data, 0);
        assert_eq!(dir.entries.len(), 1);
        let e = &dir.entries[0];
        assert_eq!(e.name, "REPORT.TXT");
        assert_eq!(e.first_cluster, 5);
        assert_eq!(e.size, 1234);
        assert!(!e.deleted);
        assert_eq!(e.times.modified.unwrap().to_string(), "2026-08-19 12:00:00");
    }

    #[test]
    fn joins_long_file_names() {
        let short = *b"LONGNA~1TXT";
        let checksum = short_name_checksum(&short);
        let name = utf16("長い名前のファイル.txt");
        let mut data = Vec::new();
        data.extend(lfn_entry(2, true, checksum, &name[13..]));
        data.extend(lfn_entry(1, false, checksum, &name[..13]));
        data.extend(short_entry(&short, 0x20, 9, 10));

        let dir = parse_directory(&data, 0);
        assert_eq!(dir.entries.len(), 1);
        assert_eq!(dir.entries[0].name, "長い名前のファイル.txt");
        assert_eq!(dir.entries[0].short_name, "LONGNA~1.TXT");
        assert!(!dir.entries[0].name_partial);
    }

    #[test]
    fn recovers_deleted_names_from_the_long_name_entries() {
        let short = *b"PHOTO1  JPG";
        let checksum = short_name_checksum(&short);
        let name = utf16("旅行の写真.jpg");
        let mut data = Vec::new();
        data.extend(lfn_entry(1, true, checksum, &name));
        data.extend(short_entry(&short, 0x20, 3, 4096));
        // 削除は全エントリの先頭バイトを 0xE5 にする。
        data[0] = DELETED_MARK;
        data[ENTRY_SIZE] = DELETED_MARK;

        let dir = parse_directory(&data, 0);
        assert_eq!(dir.entries.len(), 1);
        let e = &dir.entries[0];
        assert!(e.deleted);
        assert_eq!(e.name, "旅行の写真.jpg");
        // checksum から短い名前の先頭 1 文字も戻せる。
        assert_eq!(e.short_name, "PHOTO1.JPG");
        assert!(!e.name_partial);
    }

    #[test]
    fn marks_deleted_short_names_as_partial() {
        let mut data = short_entry(b"REPORT  TXT", 0x20, 5, 100);
        data[0] = DELETED_MARK;
        let dir = parse_directory(&data, 0);
        let e = &dir.entries[0];
        assert!(e.deleted);
        assert_eq!(e.name, "_EPORT.TXT");
        assert!(e.name_partial);
    }

    #[test]
    fn keeps_scanning_past_the_end_marker() {
        let mut data = Vec::new();
        data.extend(short_entry(b"ALIVE   TXT", 0x20, 5, 1));
        data.extend(vec![0u8; ENTRY_SIZE]); // 終端マーク
        let mut deleted = short_entry(b"GONE    TXT", 0x20, 9, 2);
        deleted[0] = DELETED_MARK;
        data.extend(deleted);

        let dir = parse_directory(&data, 0);
        assert!(dir.reached_end);
        assert_eq!(dir.entries.len(), 2);
        assert_eq!(dir.entries[1].name, "_ONE.TXT");
    }

    #[test]
    fn reads_volume_labels_and_skips_dot_entries() {
        let mut data = Vec::new();
        data.extend(short_entry(b"OFRTEST    ", ATTR_VOLUME_ID, 0, 0));
        data.extend(short_entry(b".          ", ATTR_DIRECTORY, 5, 0));
        data.extend(short_entry(b"..         ", ATTR_DIRECTORY, 0, 0));
        let dir = parse_directory(&data, 0);
        assert_eq!(dir.volume_label.as_deref(), Some("OFRTEST"));
        assert!(dir.entries.is_empty());
    }

    #[test]
    fn finds_the_end_of_a_directory() {
        let mut cluster = short_entry(b"ALIVE   TXT", 0x20, 5, 1);
        cluster.resize(4096, 0);
        assert!(has_end_marker(&cluster));

        // 隙間なく埋まっているクラスタには終端がない = 次のクラスタへ続く。
        let full: Vec<u8> = std::iter::repeat_with(|| short_entry(b"ALIVE   TXT", 0x20, 5, 1))
            .take(128)
            .flatten()
            .collect();
        assert!(!has_end_marker(&full));
    }

    #[test]
    fn detects_directory_clusters() {
        let mut cluster = Vec::new();
        cluster.extend(short_entry(b".          ", ATTR_DIRECTORY, 5, 0));
        cluster.extend(short_entry(b"..         ", ATTR_DIRECTORY, 0, 0));
        cluster.resize(4096, 0);
        assert!(looks_like_directory(&cluster));

        let mut deleted = cluster.clone();
        deleted[0] = DELETED_MARK;
        deleted[ENTRY_SIZE] = DELETED_MARK;
        assert!(looks_like_directory(&deleted));

        assert!(!looks_like_directory(&vec![0u8; 4096]));
        assert!(!looks_like_directory(&[0u8; 8]));
    }

    #[test]
    fn ignores_entries_with_impossible_attributes() {
        let mut data = short_entry(b"BROKEN  BIN", 0xC0, 5, 1);
        data[11] = 0xC0;
        assert!(parse_directory(&data, 0).entries.is_empty());
    }

    #[test]
    fn never_panics_on_random_bytes() {
        let mut data = vec![0u8; 4096];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let _ = parse_directory(&data, 0);
        let _ = looks_like_directory(&data);
        // 32 の倍数でない長さでも落ちない。
        let _ = parse_directory(&data[..4095], 0);
    }
}
