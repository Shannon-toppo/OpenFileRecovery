//! PNG のバリデータ。
//!
//! 8 バイトのシグネチャに続くチャンク列(長さ + 型 + データ + CRC)を辿り、
//! IEND チャンクの末尾を終端とする。チャンク長は自己記述なので、
//! 壊れていなければ終端は必ず正確に求まる。

use crate::format::{FileFormat, FileMetadata};
use crate::reader::Reader;
use crate::validate::{Candidate, is_ascii_tag};

/// PNG のシグネチャ。
pub(crate) const MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// チャンク長の上限(仕様上 2^31-1)。
const MAX_CHUNK: u32 = 0x7FFF_FFFF;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start, MAGIC) {
        return None;
    }

    let mut meta = FileMetadata::default();
    let mut pos = start + MAGIC.len() as u64;
    let mut end = None;
    let mut first = true;

    while pos + 12 <= limit {
        let (Some(len), Some(tag)) = (r.u32be(pos), r.array::<4>(pos + 4)) else {
            break;
        };
        if len > MAX_CHUNK || !is_ascii_tag(&tag) {
            break;
        }
        // 先頭チャンクは必ず IHDR。ここで寸法も拾う。
        if first {
            if &tag != b"IHDR" || len != 13 {
                break;
            }
            meta.width = r.u32be(pos + 8);
            meta.height = r.u32be(pos + 12);
            first = false;
        }
        let next = pos + 12 + u64::from(len);
        if next > limit {
            break;
        }
        if &tag == b"IEND" {
            end = Some(next);
            break;
        }
        pos = next;
    }

    let candidate = match end {
        Some(e) => Candidate::exact(FileFormat::Png, "png", e - start),
        None => Candidate::truncated(
            FileFormat::Png,
            "png",
            limit - start,
            pos.min(limit).saturating_sub(start),
        ),
    };
    Some(candidate.with_metadata(meta))
}
