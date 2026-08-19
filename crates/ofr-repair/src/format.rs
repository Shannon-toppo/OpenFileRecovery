//! 対応形式と、その判定。
//!
//! 修復対象は壊れたファイルなので、先頭のマジックバイトが残っているとは限らない。
//! そこで「マジック → 拡張子 → 先頭 64 KiB の特徴的なタグ探索」の順に落として判定する。
//! ヘッダが丸ごと飛んでいても、PNG なら `IDAT`、MP4 なら `mdat` のように
//! 本体側のタグが残っていることが多い。

use std::path::Path;

use crate::source::Source;

/// 特徴的なタグを探す範囲。
const PROBE_WINDOW: usize = 64 * 1024;

/// 修復できるファイル形式(PLAN.md 5.6)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RepairFormat {
    /// JPEG。
    Jpeg,
    /// PNG。
    Png,
    /// AVI(RIFF)。
    Avi,
    /// MP4 / MOV(ISO-BMFF)。構造が同じなので同じモジュールで扱う。
    Mp4,
}

impl RepairFormat {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            RepairFormat::Jpeg => "JPEG",
            RepairFormat::Png => "PNG",
            RepairFormat::Avi => "AVI",
            RepairFormat::Mp4 => "MP4 / MOV",
        }
    }

    /// JSON に出す機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            RepairFormat::Jpeg => "jpeg",
            RepairFormat::Png => "png",
            RepairFormat::Avi => "avi",
            RepairFormat::Mp4 => "mp4",
        }
    }

    /// 既定の拡張子。
    pub fn extension(self) -> &'static str {
        match self {
            RepairFormat::Jpeg => "jpg",
            RepairFormat::Png => "png",
            RepairFormat::Avi => "avi",
            RepairFormat::Mp4 => "mp4",
        }
    }

    /// 静止画か。真なら修復結果をデコードして検証できる(PLAN.md 5.6)。
    pub fn is_image(self) -> bool {
        matches!(self, RepairFormat::Jpeg | RepairFormat::Png)
    }

    /// 拡張子から判定する。
    pub fn from_extension(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "jpg" | "jpeg" | "jpe" | "jfif" => RepairFormat::Jpeg,
            "png" => RepairFormat::Png,
            "avi" => RepairFormat::Avi,
            "mp4" | "mov" | "m4v" | "qt" | "3gp" => RepairFormat::Mp4,
            _ => return None,
        })
    }
}

impl std::fmt::Display for RepairFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 中身と拡張子から形式を判定する。
pub(crate) fn detect(src: &mut Source, path: &Path) -> Option<RepairFormat> {
    if let Some(f) = by_magic(src) {
        return Some(f);
    }
    if let Some(f) = RepairFormat::from_extension(path) {
        return Some(f);
    }
    by_content(src)
}

/// 先頭のマジックバイトで判定する。ヘッダが無事なときの近道。
fn by_magic(src: &mut Source) -> Option<RepairFormat> {
    if src.matches(0, &[0xFF, 0xD8, 0xFF]) {
        return Some(RepairFormat::Jpeg);
    }
    if src.matches(0, b"\x89PNG\r\n\x1a\n") {
        return Some(RepairFormat::Png);
    }
    if src.matches(0, b"RIFF") && src.matches(8, b"AVI ") {
        return Some(RepairFormat::Avi);
    }
    if src.matches(4, b"ftyp") {
        return Some(RepairFormat::Mp4);
    }
    None
}

/// 先頭 64 KiB から、本体側に残りやすいタグを探す。
///
/// ヘッダが丸ごと消えたファイルはここで拾う。順番は「他形式と紛れにくいもの」から。
fn by_content(src: &mut Source) -> Option<RepairFormat> {
    let window = src.view(0, PROBE_WINDOW).to_vec();
    if window.is_empty() {
        return None;
    }

    // ISO-BMFF はボックス名が 4 バイト境界に並ぶ。moov / mdat は本体側にある。
    for tag in [b"moov".as_slice(), b"mdat", b"moof", b"ftyp"] {
        if find(&window, tag).is_some() {
            return Some(RepairFormat::Mp4);
        }
    }
    for tag in [b"movi".as_slice(), b"idx1", b"hdrl", b"AVI "] {
        if find(&window, tag).is_some() {
            return Some(RepairFormat::Avi);
        }
    }
    for tag in [b"IHDR".as_slice(), b"IDAT", b"IEND"] {
        if find(&window, tag).is_some() {
            return Some(RepairFormat::Png);
        }
    }
    // JPEG は最後。ヘッダが消えたエントロピー符号には特徴的なタグがなく、
    // 残っていることがあるのは Exif / JFIF の識別子か SOI くらいしかない。
    for tag in [b"Exif\0\0".as_slice(), b"JFIF\0"] {
        if find(&window, tag).is_some() {
            return Some(RepairFormat::Jpeg);
        }
    }
    if find(&window, &[0xFF, 0xD8, 0xFF]).is_some() || find(&window, &[0xFF, 0xDA]).is_some() {
        return Some(RepairFormat::Jpeg);
    }
    None
}

/// `haystack` の中から `needle` を探す。
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
