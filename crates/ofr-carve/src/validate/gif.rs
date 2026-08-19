//! GIF のバリデータ。
//!
//! ヘッダ → 論理画面記述子 → (大域カラーテーブル) → ブロック列 と辿り、
//! トレーラ(`0x3B`)を終端とする。拡張ブロックと画像データはどちらも
//! 「長さ 1 バイト + データ」のサブブロック列で終端が自己記述なので、
//! 素直に数えていけば終端は正確に出る。

use crate::format::{FileFormat, FileMetadata};
use crate::reader::Reader;
use crate::validate::Candidate;

/// ブロックを辿る回数の上限。壊れたデータで長時間回らないようにする。
const MAX_BLOCKS: u32 = 100_000;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    let header = r.array::<6>(start)?;
    if &header != b"GIF87a" && &header != b"GIF89a" {
        return None;
    }

    let meta = FileMetadata {
        width: r.u16le(start + 6).map(u32::from).filter(|v| *v > 0),
        height: r.u16le(start + 8).map(u32::from).filter(|v| *v > 0),
        ..FileMetadata::default()
    };
    if meta.width.is_none() || meta.height.is_none() {
        return None;
    }

    // 論理画面記述子は 7 バイト。その packed フィールドに大域カラーテーブルの有無が入る。
    let packed = r.u8(start + 10)?;
    let mut pos = start + 13;
    if packed & 0x80 != 0 {
        pos += 3 * (1u64 << ((packed & 0x07) + 1));
    }

    let mut end = None;
    for _ in 0..MAX_BLOCKS {
        if pos >= limit {
            break;
        }
        match r.u8(pos) {
            // トレーラ。
            Some(0x3B) => {
                end = Some(pos + 1);
                break;
            }
            // 拡張ブロック: 導入バイト + ラベル + サブブロック列。
            Some(0x21) => {
                pos += 2;
                match skip_sub_blocks(r, pos, limit) {
                    Some(next) => pos = next,
                    None => break,
                }
            }
            // 画像記述子: 9 バイト + (局所カラーテーブル) + LZW 最小符号長 + サブブロック列。
            Some(0x2C) => {
                let Some(local) = r.u8(pos + 9) else { break };
                pos += 10;
                if local & 0x80 != 0 {
                    pos += 3 * (1u64 << ((local & 0x07) + 1));
                }
                pos += 1; // LZW 最小符号長
                match skip_sub_blocks(r, pos, limit) {
                    Some(next) => pos = next,
                    None => break,
                }
            }
            _ => break,
        }
    }

    let candidate = match end {
        Some(e) => Candidate::exact(FileFormat::Gif, "gif", e - start),
        None => Candidate::truncated(
            FileFormat::Gif,
            "gif",
            limit - start,
            pos.min(limit).saturating_sub(start),
        ),
    };
    Some(candidate.with_metadata(meta))
}

/// 「長さ + データ」の連なりを、長さ 0 のブロックまで読み飛ばす。
fn skip_sub_blocks(r: &mut Reader<'_>, from: u64, limit: u64) -> Option<u64> {
    let mut pos = from;
    for _ in 0..MAX_BLOCKS {
        if pos >= limit {
            return None;
        }
        match r.u8(pos)? {
            0 => return Some(pos + 1),
            n => pos += 1 + u64::from(n),
        }
    }
    None
}
