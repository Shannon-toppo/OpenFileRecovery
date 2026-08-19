//! 範囲外でも panic しないリトルエンディアン読み出し。
//!
//! 壊れたバイト列を舐めるパーサでは、範囲チェック漏れがそのまま panic になる。
//! スライス添字の代わりにここを通すことで、足りない所は 0 として扱われる。

/// `off` から 1 バイト。範囲外なら 0。
pub fn u8_at(buf: &[u8], off: usize) -> u8 {
    buf.get(off).copied().unwrap_or(0)
}

/// `off` から u16(LE)。範囲外なら 0。
pub fn u16_at(buf: &[u8], off: usize) -> u16 {
    match buf.get(off..off + 2) {
        Some(b) => u16::from_le_bytes([b[0], b[1]]),
        None => 0,
    }
}

/// `off` から u32(LE)。範囲外なら 0。
pub fn u32_at(buf: &[u8], off: usize) -> u32 {
    match buf.get(off..off + 4) {
        Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

/// `off` から u64(LE)。範囲外なら 0。
pub fn u64_at(buf: &[u8], off: usize) -> u64 {
    match buf.get(off..off + 8) {
        Some(b) => u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        None => 0,
    }
}

/// UTF-16LE の並びを文字列にする。`0x0000` で打ち切る。
///
/// 不正なサロゲートは置換文字にする(復元したファイル名は表示できることが優先で、
/// バイト単位の忠実さは要らない)。
pub fn utf16le_string(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    char::decode_utf16(units[..end].iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// OEM コードページの 8.3 名を表示用の文字列にする。
///
/// FAT の短い名前は OS の OEM コードページ(日本語 Windows なら Shift_JIS)で
/// 記録されているが、どのコードページかはボリュームに書かれていない。ASCII 範囲を
/// そのまま使い、それ以外は `?` にする。長い名前(LFN)がある項目では
/// そちらが優先されるので、これが効くのは 8.3 名しか残っていない項目だけ。
pub fn oem_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii() && b >= 0x20 {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_within_and_past_the_end() {
        let buf = [0x01, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(u16_at(&buf, 0), 0x0201);
        assert_eq!(u32_at(&buf, 1), 0x0504_0302);
        assert_eq!(u32_at(&buf, 2), 0);
        assert_eq!(u64_at(&buf, 0), 0);
        assert_eq!(u8_at(&buf, 99), 0);
    }

    #[test]
    fn decodes_utf16_names() {
        let units = [0x0041u16, 0x3042, 0x0000, 0x0042];
        assert_eq!(utf16le_string(&units), "Aあ");
        assert_eq!(utf16le_string(&[0xD800, 0x0041]), "\u{fffd}A");
    }

    #[test]
    fn maps_non_ascii_oem_bytes_to_question_marks() {
        assert_eq!(oem_string(b"TEST    TXT"), "TEST    TXT");
        assert_eq!(oem_string(&[0x82, 0xA0, b'A']), "??A");
    }
}
