//! ISO-BMFF(MP4 / MOV / HEIC)のバリデータ。
//!
//! ISO-BMFF はファイル全体がボックスの列で、各ボックスが自分の長さを持つ。
//! なので先頭の `ftyp` からボックスを順に足していけば終端は正確に出る
//! (PLAN.md 5.4「ヘッダ内のサイズ情報を辿って正確な終端を計算する」)。
//!
//! 種別は `ftyp` のブランドで決める。`qt  ` なら MOV、`heic` 系なら HEIC、
//! それ以外の既知ブランドは MP4 系として扱う。

use crate::format::{FileFormat, FileMetadata, Timestamp};
use crate::reader::Reader;
use crate::validate::{Candidate, is_ascii_tag};

/// `ftyp` はファイル先頭の 4 バイト(ボックス長)の次に来る。
pub(crate) const MAGIC_OFFSET: u64 = 4;

/// 歩くボックス数の上限。
const MAX_BOXES: u32 = 100_000;

/// 1904-01-01 から 1970-01-01 までの秒数。ISO-BMFF の時刻はこの起点。
const EPOCH_1904_TO_1970: i64 = 2_082_844_800;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start + MAGIC_OFFSET, b"ftyp") {
        return None;
    }
    let ftyp_size = u64::from(r.u32be(start)?);
    if !(12..=1024).contains(&ftyp_size) {
        return None;
    }
    let brand = r.array::<4>(start + 8)?;
    let (format, ext) = brand_to_format(&brand).or_else(|| {
        // 主ブランドが未知でも、互換ブランド欄に既知のものがあれば拾う。
        let mut at = start + 16;
        while at + 4 <= start + ftyp_size {
            if let Some(b) = r.array::<4>(at)
                && let Some(hit) = brand_to_format(&b)
            {
                return Some(hit);
            }
            at += 4;
        }
        None
    })?;

    let mut meta = FileMetadata::default();
    let mut pos = start;
    let mut has_payload = false;
    let mut truncated = false;

    for i in 0..MAX_BOXES {
        if pos >= limit {
            truncated = pos > limit;
            break;
        }
        let (Some(size32), Some(tag)) = (r.u32be(pos), r.array::<4>(pos + 4)) else {
            break;
        };
        if !is_ascii_tag(&tag) {
            break;
        }
        // 先頭は必ず ftyp。2 つ目以降で未知のタグが出たらそこがファイルの終わり。
        if i == 0 && &tag != b"ftyp" {
            return None;
        }

        let (header, size) = match size32 {
            // 1 なら 64 ビット長が続く。
            1 => (16u64, r.u64be(pos + 8)?),
            // 0 は「ファイル末尾まで」。終端はサイズ情報から確定できない。
            0 => {
                pos = limit;
                truncated = true;
                break;
            }
            n => (8u64, u64::from(n)),
        };
        if size < header {
            break;
        }

        match &tag {
            b"moov" => {
                has_payload = true;
                read_mvhd(r, pos + header, pos + size, &mut meta);
            }
            b"mdat" | b"meta" | b"moof" | b"mdia" => has_payload = true,
            _ => {}
        }

        let Some(next) = pos.checked_add(size) else {
            break;
        };
        if next > limit {
            truncated = true;
            pos = limit;
            break;
        }
        pos = next;
    }

    // ftyp しかない(中身のボックスが 1 つもない)ものは偶然の一致とみなす。
    if !has_payload {
        return None;
    }

    let candidate = if truncated {
        Candidate::truncated(format, ext, limit - start, ftyp_size)
    } else {
        Candidate::exact(format, ext, pos - start)
    };
    Some(candidate.with_metadata(meta))
}

/// `ftyp` のブランドから形式と拡張子を決める。
fn brand_to_format(brand: &[u8; 4]) -> Option<(FileFormat, &'static str)> {
    Some(match brand {
        b"qt  " => (FileFormat::Mov, "mov"),
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => {
            (FileFormat::Heic, "heic")
        }
        b"M4A " => (FileFormat::Mp4, "m4a"),
        b"M4V " => (FileFormat::Mp4, "m4v"),
        b"isom" | b"iso2" | b"iso4" | b"iso5" | b"iso6" | b"mp41" | b"mp42" | b"avc1" | b"mmp4"
        | b"dash" | b"MSNV" | b"M4P " => (FileFormat::Mp4, "mp4"),
        b"3gp4" | b"3gp5" | b"3gp6" | b"3gp7" | b"3g2a" => (FileFormat::Mp4, "3gp"),
        _ => return None,
    })
}

/// 指定範囲の直下から名前の一致するボックスを探す。
fn find_box(r: &mut Reader<'_>, name: &[u8; 4], from: u64, end: u64) -> Option<u64> {
    let mut pos = from;
    for _ in 0..1024 {
        if pos + 8 > end {
            return None;
        }
        let (Some(size), Some(tag)) = (r.u32be(pos), r.array::<4>(pos + 4)) else {
            return None;
        };
        if size < 8 || !is_ascii_tag(&tag) {
            return None;
        }
        if &tag == name {
            return Some(pos);
        }
        pos = pos.checked_add(u64::from(size))?;
    }
    None
}

/// `moov` の中の `mvhd` から作成日時と再生時間を拾う。
fn read_mvhd(r: &mut Reader<'_>, from: u64, end: u64, meta: &mut FileMetadata) {
    let Some(mvhd) = find_box(r, b"mvhd", from, end) else {
        return;
    };
    let body = mvhd + 8;
    let Some(version) = r.u8(body) else { return };
    let (created, timescale, duration) = if version == 1 {
        (
            r.u64be(body + 4).map(|v| v as i64),
            r.u32be(body + 20),
            r.u64be(body + 24),
        )
    } else {
        (
            r.u32be(body + 4).map(i64::from),
            r.u32be(body + 12),
            r.u32be(body + 16).map(u64::from),
        )
    };

    if let Some(secs) = created
        && secs > 0
        && let Some(ts) = Timestamp::from_unix_seconds(secs - EPOCH_1904_TO_1970)
        && ts.is_valid()
    {
        meta.timestamp = Some(ts);
    }
    if let (Some(scale), Some(dur)) = (timescale, duration)
        && scale > 0
    {
        meta.duration_ms = Some(dur.saturating_mul(1000) / u64::from(scale));
    }
}
