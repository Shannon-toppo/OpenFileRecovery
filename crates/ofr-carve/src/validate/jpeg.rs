//! JPEG のバリデータ。
//!
//! SOI から始めてマーカー列を辿り、SOS 以降のエントロピー符号は
//! バイトスタッフィング(`FF 00`)とリスタートマーカー(`FF D0`〜`FF D7`)を
//! 読み飛ばしながら EOI(`FF D9`)を探す。プログレッシブ JPEG のように
//! SOS が複数あるものは、エントロピー走査が次のマーカーで止まって
//! マーカー走査に戻ることで自然に扱える。

use crate::exif;
use crate::format::{FileFormat, FileMetadata};
use crate::reader::Reader;
use crate::validate::Candidate;

/// SOI 直後に来てよいマーカー。ここで弾くと `FF D8 FF` の偶然一致を落とせる。
fn plausible_first_marker(marker: u8) -> bool {
    matches!(marker,
        0xC0..=0xCF | 0xDB | 0xDD | 0xDA | 0xE0..=0xEF | 0xFE)
}

/// SOF(フレームヘッダ)マーカーか。DHT(C4)/JPG(C8)/DAC(CC)は除く。
fn is_sof(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
}

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    if !r.matches(start, &[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    if !plausible_first_marker(r.u8(start + 3)?) {
        return None;
    }

    let mut meta = FileMetadata::default();
    let mut pos = start + 2;
    let mut seen_sof = false;
    let mut seen_sos = false;
    let mut end: Option<u64> = None;
    // 「確実にこの JPEG のもの」と言える位置。SOS より後ろはエントロピー符号で、
    // 終端マーカーを失ったファイルではマーカー走査が後続の雑音に迷い込むため、
    // 検証済みとして数えるのは SOS ヘッダまでに留める。
    let mut verified = start + 2;

    while pos + 2 <= limit {
        let Some(0xFF) = r.u8(pos) else { break };
        let Some(marker) = r.u8(pos + 1) else { break };

        match marker {
            // フィルバイト。マーカーの前にいくつ入っていてもよい。
            0xFF => {
                pos += 1;
                continue;
            }
            // 長さフィールドを持たないマーカー。
            0x01 | 0xD0..=0xD8 => {
                pos += 2;
                continue;
            }
            // EOI: ここが終端。
            0xD9 => {
                end = Some(pos + 2);
                break;
            }
            0xDA => {
                let Some(len) = r.u16be(pos + 2) else { break };
                if len < 2 {
                    break;
                }
                seen_sos = true;
                pos += 2 + u64::from(len);
                verified = pos;
                match scan_entropy(r, pos, limit) {
                    Some(next) => pos = next,
                    None => break,
                }
            }
            _ => {
                let Some(len) = r.u16be(pos + 2) else { break };
                if len < 2 {
                    break;
                }
                let seg = pos + 4; // セグメント本体(長さフィールドの次)
                let body = u64::from(len) - 2;

                if is_sof(marker) && len >= 8 {
                    seen_sof = true;
                    meta.height = r.u16be(pos + 5).map(u32::from);
                    meta.width = r.u16be(pos + 7).map(u32::from);
                } else if marker == 0xE1 {
                    exif::read_app1(r, seg, body, &mut meta);
                }
                pos += 2 + u64::from(len);
                if !seen_sos {
                    verified = pos;
                }
            }
        }
    }

    // SOF も SOS も見つからないものは JPEG とみなさない。
    if !seen_sof && !seen_sos {
        return None;
    }

    let candidate = match end {
        Some(e) => Candidate::exact(FileFormat::Jpeg, "jpg", e - start),
        None => Candidate::truncated(
            FileFormat::Jpeg,
            "jpg",
            limit - start,
            verified.min(limit) - start,
        ),
    };
    Some(candidate.with_metadata(meta))
}

/// エントロピー符号を読み飛ばし、次のマーカー位置(`FF` のある位置)を返す。
fn scan_entropy(r: &mut Reader<'_>, from: u64, limit: u64) -> Option<u64> {
    let mut pos = from;
    while pos < limit {
        let p = r.find_byte(0xFF, pos, limit)?;
        match r.u8(p + 1)? {
            // バイトスタッフィング。データ中の 0xFF は FF 00 と書かれる。
            0x00 => pos = p + 2,
            // リスタートマーカー。エントロピー符号の途中に現れる。
            0xD0..=0xD7 => pos = p + 2,
            // フィルバイト。次のバイトを見直す。
            0xFF => pos = p + 1,
            // それ以外は本物のマーカー。呼び出し側のマーカー走査に戻す。
            _ => return Some(p),
        }
    }
    None
}
