//! シグネチャ定義テーブル。
//!
//! 対応形式を増やすときはこの表に 1 行足すだけで済むようにしてある
//! (PLAN.md 5.4「定義はテーブル駆動にして追加しやすくする」)。
//!
//! 走査は 2 段構え。まず [`memchr`] のサブストリング検索でマジックバイトの
//! 候補位置を拾い(SIMD が効く)、当たった位置だけをフォーマット別の
//! バリデータに渡して本物か確かめる。

use crate::format::FileFormat;
use crate::reader::Reader;
use crate::validate::{Candidate, gif, isobmff, jpeg, mp3, pdf, png, riff, zip};

/// バリデータの関数型。
///
/// 引数は「読み出し口」「ファイル先頭の位置」「終端としてありうる最大位置」。
pub(crate) type ProbeFn = fn(&mut Reader<'_>, u64, u64) -> Option<Candidate>;

/// 1 つのマジックバイト定義。
pub struct Signature {
    /// 表示用の名前。
    pub name: &'static str,
    /// 探すバイト列。
    pub magic: &'static [u8],
    /// マジックがファイル先頭から何バイト目に現れるか(ISO-BMFF は 4)。
    pub magic_offset: u64,
    /// このシグネチャから出うる形式。`--formats` での絞り込みに使う。
    pub formats: &'static [FileFormat],
    /// この形式として切り出す最大サイズ。
    pub max_size: u64,
    pub(crate) probe: ProbeFn,
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signature")
            .field("name", &self.name)
            .field("magic", &self.magic)
            .field("magic_offset", &self.magic_offset)
            .finish_non_exhaustive()
    }
}

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// 対応する全シグネチャ。
pub static SIGNATURES: &[Signature] = &[
    Signature {
        name: "JPEG",
        magic: &[0xFF, 0xD8, 0xFF],
        magic_offset: 0,
        formats: &[FileFormat::Jpeg],
        max_size: 256 * MIB,
        probe: jpeg::probe,
    },
    Signature {
        name: "PNG",
        magic: png::MAGIC,
        magic_offset: 0,
        formats: &[FileFormat::Png],
        max_size: 512 * MIB,
        probe: png::probe,
    },
    Signature {
        name: "GIF87a",
        magic: b"GIF87a",
        magic_offset: 0,
        formats: &[FileFormat::Gif],
        max_size: 64 * MIB,
        probe: gif::probe,
    },
    Signature {
        name: "GIF89a",
        magic: b"GIF89a",
        magic_offset: 0,
        formats: &[FileFormat::Gif],
        max_size: 64 * MIB,
        probe: gif::probe,
    },
    Signature {
        name: "ISO-BMFF",
        magic: b"ftyp",
        magic_offset: isobmff::MAGIC_OFFSET,
        formats: &[FileFormat::Mp4, FileFormat::Mov, FileFormat::Heic],
        max_size: 16 * GIB,
        probe: isobmff::probe,
    },
    Signature {
        name: "RIFF",
        magic: b"RIFF",
        magic_offset: 0,
        formats: &[FileFormat::Avi, FileFormat::Wav],
        max_size: 4 * GIB,
        probe: riff::probe,
    },
    Signature {
        name: "ZIP",
        magic: b"PK\x03\x04",
        magic_offset: 0,
        formats: &[FileFormat::Zip],
        max_size: 8 * GIB,
        probe: zip::probe,
    },
    Signature {
        name: "PDF",
        magic: b"%PDF-",
        magic_offset: 0,
        formats: &[FileFormat::Pdf],
        max_size: 2 * GIB,
        probe: pdf::probe,
    },
    Signature {
        name: "ID3",
        magic: b"ID3",
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
    // ID3 タグのない MP3。フレーム同期は 11 ビットしかないので、
    // よく使われる組み合わせだけを並べて候補を絞る(残りはバリデータが弾く)。
    Signature {
        name: "MP3 (MPEG1 Layer3)",
        magic: &[0xFF, 0xFB],
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
    Signature {
        name: "MP3 (MPEG1 Layer3, CRC)",
        magic: &[0xFF, 0xFA],
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
    Signature {
        name: "MP3 (MPEG2 Layer3)",
        magic: &[0xFF, 0xF3],
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
    Signature {
        name: "MP3 (MPEG2 Layer3, CRC)",
        magic: &[0xFF, 0xF2],
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
    Signature {
        name: "MP3 (MPEG2.5 Layer3)",
        magic: &[0xFF, 0xE3],
        magic_offset: 0,
        formats: &[FileFormat::Mp3],
        max_size: 512 * MIB,
        probe: mp3::probe,
    },
];

/// マジックバイトがファイル先頭から離れている最大距離。
///
/// 走査窓の継ぎ目でシグネチャを取りこぼさないための重なり幅に使う。
pub(crate) fn max_magic_span() -> usize {
    SIGNATURES
        .iter()
        .map(|s| s.magic_offset as usize + s.magic.len())
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_has_at_least_one_signature() {
        for f in FileFormat::all() {
            assert!(
                SIGNATURES.iter().any(|s| s.formats.contains(f)),
                "{f} のシグネチャがない"
            );
        }
    }

    #[test]
    fn magics_are_usable_for_scanning() {
        for s in SIGNATURES {
            assert!(!s.magic.is_empty(), "{}: マジックが空", s.name);
            assert!(s.magic.len() >= 2, "{}: マジックが短すぎる", s.name);
            assert!(s.max_size > 0, "{}: 最大サイズが 0", s.name);
        }
        assert_eq!(max_magic_span(), 8);
    }
}
