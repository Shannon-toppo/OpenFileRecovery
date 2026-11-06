//! RIFF(AVI / WAV / WebP)のバリデータ。
//!
//! RIFF はヘッダ(`RIFF` + サイズ + フォーム種別)にファイル全体の長さが
//! 入っているので、終端はそこから直接求まる。サイズが壊れている場合に備えて
//! チャンク列も歩き、宣言サイズと辻褄が合うかを確かめる。

use crate::format::{FileFormat, FileMetadata};
use crate::reader::Reader;
use crate::validate::{Candidate, is_ascii_tag};

/// 歩くチャンク数の上限。
const MAX_CHUNKS: u32 = 100_000;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start, b"RIFF") {
        return None;
    }
    let size = u64::from(r.u32le(start + 4)?);
    let form = r.array::<4>(start + 8)?;
    let (format, ext) = match &form {
        b"AVI " | b"AVIX" => (FileFormat::Avi, "avi"),
        b"WAVE" => (FileFormat::Wav, "wav"),
        b"WEBP" => (FileFormat::Webp, "webp"),
        _ => return None,
    };
    // RIFF ヘッダ 8 バイト + フォーム種別 4 バイトが最小。
    if size < 4 {
        return None;
    }

    let declared_end = start.saturating_add(8).saturating_add(size);
    let mut meta = FileMetadata::default();
    let walk_end = walk_chunks(r, start + 12, declared_end.min(limit), format, &mut meta);

    // 宣言サイズが収まっていて、チャンク列もそこまで綺麗に並んでいれば終端は確定。
    let candidate = if declared_end <= limit && walk_end == declared_end {
        Candidate::exact(format, ext, declared_end - start)
    } else {
        Candidate::truncated(
            format,
            ext,
            declared_end.min(limit) - start,
            walk_end.min(limit).saturating_sub(start).max(12),
        )
    };
    Some(candidate.with_metadata(meta))
}

/// チャンク列を歩いて、綺麗に並んでいた最後の位置を返す。
///
/// ついでに AVI なら `avih`、WAV なら `fmt ` / `data`、WebP なら画像チャンクから
/// メタデータを拾う。
fn walk_chunks(
    r: &mut Reader<'_>,
    from: u64,
    end: u64,
    format: FileFormat,
    meta: &mut FileMetadata,
) -> u64 {
    let mut pos = from;
    let mut byte_rate: Option<u32> = None;
    let mut data_size: Option<u32> = None;

    for _ in 0..MAX_CHUNKS {
        if pos == end {
            break;
        }
        if pos + 8 > end {
            break;
        }
        let (Some(tag), Some(size)) = (r.array::<4>(pos), r.u32le(pos + 4)) else {
            break;
        };
        if !is_ascii_tag(&tag) {
            break;
        }
        let body = pos + 8;
        match &tag {
            // LIST は 4 バイトのフォーム種別のあとに子チャンクが並ぶ。
            b"LIST" if format == FileFormat::Avi => {
                if r.matches(body, b"hdrl") && r.matches(body + 4, b"avih") {
                    read_avih(r, body + 12, meta);
                }
            }
            b"fmt " if format == FileFormat::Wav && size >= 16 => {
                byte_rate = r.u32le(body + 8).filter(|v| *v > 0);
            }
            b"data" if format == FileFormat::Wav => {
                data_size = Some(size);
            }
            b"VP8X" | b"VP8L" | b"VP8 " if format == FileFormat::Webp && meta.width.is_none() => {
                read_webp_size(r, &tag, body, meta);
            }
            _ => {}
        }
        // チャンクは 2 バイト境界に揃うので、奇数長なら 1 バイトのパディングが入る。
        let advance = 8 + u64::from(size) + u64::from(size & 1);
        let Some(next) = pos.checked_add(advance) else {
            break;
        };
        if next > end {
            break;
        }
        pos = next;
    }

    if let (Some(rate), Some(bytes)) = (byte_rate, data_size) {
        meta.duration_ms = Some(u64::from(bytes) * 1000 / u64::from(rate));
    }
    pos
}

/// AVI のメインヘッダ(`avih`)から寸法と長さを拾う。
fn read_avih(r: &mut Reader<'_>, at: u64, meta: &mut FileMetadata) {
    let micros_per_frame = r.u32le(at);
    let total_frames = r.u32le(at + 16);
    meta.width = r.u32le(at + 32).filter(|v| *v > 0);
    meta.height = r.u32le(at + 36).filter(|v| *v > 0);
    if let (Some(us), Some(frames)) = (micros_per_frame, total_frames)
        && us > 0
        && frames > 0
    {
        meta.duration_ms = Some(u64::from(us) * u64::from(frames) / 1000);
    }
}

/// WebP の画像チャンクから寸法を拾う。
///
/// 拡張形式(`VP8X`)ならキャンバス寸法、単体の画像なら
/// 可逆(`VP8L`)/非可逆(`VP8 `)のビットストリーム先頭から読む。
fn read_webp_size(r: &mut Reader<'_>, tag: &[u8; 4], body: u64, meta: &mut FileMetadata) {
    let u24le = |b: [u8; 3]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
    let size = match tag {
        // フラグ 1 バイト + 予約 3 バイトのあと、幅-1 と高さ-1 が 24 ビットずつ。
        b"VP8X" => r
            .array::<3>(body + 4)
            .zip(r.array::<3>(body + 7))
            .map(|(w, h)| (u24le(w) + 1, u24le(h) + 1)),
        // 署名 0x2F のあと、幅-1 と高さ-1 が 14 ビットずつ詰めて入る。
        b"VP8L" if r.u8(body) == Some(0x2F) => r
            .u32le(body + 1)
            .map(|v| ((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1)),
        // フレームタグ 3 バイト + 開始コード 9D 01 2A のあと、14 ビットの幅と高さ。
        b"VP8 " if r.matches(body + 3, &[0x9D, 0x01, 0x2A]) => r
            .u16le(body + 6)
            .zip(r.u16le(body + 8))
            .map(|(w, h)| (u32::from(w & 0x3FFF), u32::from(h & 0x3FFF))),
        _ => None,
    };
    if let Some((w, h)) = size
        && w > 0
        && h > 0
    {
        meta.width = Some(w);
        meta.height = Some(h);
    }
}
