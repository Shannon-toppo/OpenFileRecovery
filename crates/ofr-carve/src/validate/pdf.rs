//! PDF のバリデータ。
//!
//! `%PDF-` で始まり `%%EOF` で終わる。増分更新された PDF には `%%EOF` が
//! 複数あるので、次の `%PDF-` が現れるまでは同じファイルの続きとみなして
//! 最後の `%%EOF` まで伸ばす。

use crate::format::{FileFormat, FileMetadata, Timestamp};
use crate::reader::Reader;
use crate::validate::Candidate;

/// `/CreationDate` を探す範囲。文書の先頭側にあることが多い。
const METADATA_SCAN: u64 = 256 * 1024;

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start, b"%PDF-") {
        return None;
    }
    // バージョンは `1.0`〜`2.0`。
    let major = r.u8(start + 5)?;
    if !(b'1'..=b'2').contains(&major) || r.u8(start + 6)? != b'.' {
        return None;
    }

    let mut end = None;
    let mut from = start + 5;
    while let Some(eof) = r.find(b"%%EOF", from, limit) {
        // 直前の %%EOF との間に別の PDF が始まっていたら、そこで打ち切る。
        if let Some(prev) = end
            && r.find(b"%PDF-", prev, eof).is_some()
        {
            break;
        }
        // 末尾の改行も含めておく。
        let mut e = eof + 5;
        while matches!(r.u8(e), Some(b'\r') | Some(b'\n')) && e < limit {
            e += 1;
        }
        end = Some(e);
        from = eof + 5;
    }

    let mut meta = FileMetadata::default();
    read_creation_date(r, start, limit.min(start + METADATA_SCAN), &mut meta);

    let candidate = match end {
        Some(e) => Candidate::exact(FileFormat::Pdf, "pdf", e - start),
        None => Candidate::truncated(FileFormat::Pdf, "pdf", limit - start, 8),
    };
    Some(candidate.with_metadata(meta))
}

/// `/CreationDate (D:20230415142530+09'00')` から日時を拾う。
fn read_creation_date(r: &mut Reader<'_>, start: u64, until: u64, meta: &mut FileMetadata) {
    let Some(at) = r.find(b"/CreationDate", start, until) else {
        return;
    };
    let head = r.view(at, 64);
    let Some(d) = head.windows(2).position(|w| w == b"D:") else {
        return;
    };
    let digits: Vec<u8> = head[d + 2..]
        .iter()
        .copied()
        .take_while(u8::is_ascii_digit)
        .collect();
    if digits.len() < 14 {
        return;
    }
    let num = |a: usize, b: usize| -> Option<u32> {
        std::str::from_utf8(&digits[a..b]).ok()?.parse().ok()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        num(0, 4),
        num(4, 6),
        num(6, 8),
        num(8, 10),
        num(10, 12),
        num(12, 14),
    ) else {
        return;
    };
    let ts = Timestamp {
        year: year as i32,
        month,
        day,
        hour,
        minute,
        second,
    };
    if ts.is_valid() {
        meta.timestamp = Some(ts);
    }
}
