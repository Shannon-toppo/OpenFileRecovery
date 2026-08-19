//! ZIP のバリデータ。
//!
//! ZIP の終端は末尾の EOCD(End of Central Directory)レコードで、そこに
//! 中央ディレクトリの位置と大きさが入っている。ローカルヘッダを 1 つずつ
//! 歩くよりも、EOCD を探して整合を確かめる方が確実で速い
//! (データ記述子を使うストリーム書き出しの ZIP でも正しく終端が出る)。
//!
//! docx / xlsx / pptx は ZIP なので、中央ディレクトリに載っているパス名を見て
//! 拡張子を決める。

use crate::format::FileFormat;
use crate::reader::Reader;
use crate::validate::Candidate;

/// ローカルファイルヘッダ。
const LOCAL_HEADER: &[u8] = b"PK\x03\x04";
/// 中央ディレクトリのエントリ。
const CENTRAL_HEADER: &[u8] = b"PK\x01\x02";
/// EOCD レコード。
const EOCD: &[u8] = b"PK\x05\x06";
/// 歩くローカルヘッダ数の上限。
const MAX_ENTRIES: u32 = 200_000;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start, LOCAL_HEADER) {
        return None;
    }
    // バージョンと汎用フラグが常識的な範囲か見て、偶然の一致を落とす。
    if r.u16le(start + 4)? > 100 {
        return None;
    }

    if let Some((eocd, end)) = find_eocd(r, start, limit) {
        let ext = detect_ooxml(r, start, eocd).unwrap_or("zip");
        return Some(Candidate::exact(FileFormat::Zip, ext, end - start));
    }

    // EOCD が見つからない(壊れている / 上限を超えた)場合は、
    // ローカルヘッダを歩けた所までを確定分にする。
    let walked = walk_local_headers(r, start, limit);
    Some(Candidate::truncated(
        FileFormat::Zip,
        "zip",
        limit - start,
        walked.saturating_sub(start).max(30),
    ))
}

/// 整合の取れる EOCD を探し、その位置とファイル終端を返す。
fn find_eocd(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<(u64, u64)> {
    let mut from = start;
    while let Some(eocd) = r.find(EOCD, from, limit) {
        from = eocd + 4;
        let (Some(cd_size), Some(cd_offset), Some(comment_len)) =
            (r.u32le(eocd + 12), r.u32le(eocd + 16), r.u16le(eocd + 20))
        else {
            return None;
        };
        let end = eocd + 22 + u64::from(comment_len);
        if end > limit {
            return None;
        }
        // 中央ディレクトリの位置が辻褄が合うか確かめる。Zip64 では 0xFFFFFFFF に
        // 逃がされているので、その場合は EOCD の位置だけを信じる。
        let zip64 = cd_offset == u32::MAX || cd_size == u32::MAX;
        let consistent = zip64
            || (u64::from(cd_size) <= eocd - start
                && start + u64::from(cd_offset) + u64::from(cd_size) == eocd
                && (cd_size == 0 || r.matches(start + u64::from(cd_offset), CENTRAL_HEADER)));
        if consistent {
            return Some((eocd, end));
        }
    }
    None
}

/// 中央ディレクトリのパス名から OOXML の種類を見分ける。
fn detect_ooxml(r: &mut Reader<'_>, start: u64, eocd: u64) -> Option<&'static str> {
    // 中央ディレクトリだけを見れば全エントリのパス名が並んでいる。
    let cd_size = u64::from(r.u32le(eocd + 12)?);
    let cd_start = eocd.checked_sub(cd_size)?.max(start);
    if !r.matches(start + 30, b"[Content_Types].xml") && !r.matches(start + 30, b"_rels/") {
        return None;
    }
    for (needle, ext) in [
        (&b"word/document.xml"[..], "docx"),
        (&b"xl/workbook.xml"[..], "xlsx"),
        (&b"ppt/presentation.xml"[..], "pptx"),
    ] {
        if r.find(needle, cd_start, eocd).is_some() {
            return Some(ext);
        }
    }
    None
}

/// ローカルヘッダを順に歩いて、確実にこの書庫の一部だと言える位置まで進む。
fn walk_local_headers(r: &mut Reader<'_>, start: u64, limit: u64) -> u64 {
    let mut pos = start;
    for _ in 0..MAX_ENTRIES {
        if !r.matches(pos, LOCAL_HEADER) {
            break;
        }
        let (Some(flags), Some(compressed), Some(name_len), Some(extra_len)) = (
            r.u16le(pos + 6),
            r.u32le(pos + 18),
            r.u16le(pos + 26),
            r.u16le(pos + 28),
        ) else {
            break;
        };
        // データ記述子を使うものはローカルヘッダに大きさが入っていない。
        if flags & 0x08 != 0 && compressed == 0 {
            pos += 30 + u64::from(name_len) + u64::from(extra_len);
            break;
        }
        let next = pos + 30 + u64::from(name_len) + u64::from(extra_len) + u64::from(compressed);
        if next > limit {
            break;
        }
        pos = next;
    }
    pos
}
