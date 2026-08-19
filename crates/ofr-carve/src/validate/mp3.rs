//! MP3 のバリデータ。
//!
//! MP3 には終端マーカーがないので、フレームヘッダを 1 つずつ読んで長さを足し、
//! フレームとして解釈できなくなった所を終端とする。先頭の ID3v2 タグと
//! 末尾の ID3v1 タグ(`TAG` + 128 バイト)も含める。
//!
//! フレーム同期(`FF Ex`〜`FF Fx`)は 11 ビットしかなく偶然当たりやすいので、
//! ID3 なしで始まるものは連続する有効フレームを [`MIN_BARE_FRAMES`] 個
//! 要求して誤検出を落とす。

use crate::format::FileFormat;
use crate::reader::Reader;
use crate::validate::Candidate;

/// ID3v2 なしで始まる場合に要求する連続フレーム数。
const MIN_BARE_FRAMES: u32 = 8;
/// ID3v2 から始まる場合に要求する連続フレーム数。
const MIN_TAGGED_FRAMES: u32 = 1;
/// 歩くフレーム数の上限。
const MAX_FRAMES: u32 = 2_000_000;

/// ビットレート表(kbps)。[MPEG1 Layer3, MPEG2/2.5 Layer3]。
const BITRATES_V1_L3: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const BITRATES_V2_L3: [u32; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];
/// サンプリング周波数表(Hz)。[MPEG1, MPEG2, MPEG2.5]。
const SAMPLE_RATES: [[u32; 3]; 3] = [
    [44100, 48000, 32000],
    [22050, 24000, 16000],
    [11025, 12000, 8000],
];

/// 1 フレームのヘッダから読み取った情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    length: u64,
    sample_rate: u32,
    samples: u32,
    version: u8,
    layer: u8,
}

pub(crate) fn probe(r: &mut Reader<'_>, start: u64, limit: u64) -> Option<Candidate> {
    let (audio_start, tagged) = match id3v2_size(r, start) {
        Some(size) => (start + size, true),
        None => (start, false),
    };

    let mut pos = audio_start;
    let mut frames = 0u32;
    let mut first: Option<Frame> = None;
    let mut total_samples = 0u64;

    while pos < limit && frames < MAX_FRAMES {
        let Some(header) = r.array::<4>(pos) else {
            break;
        };
        let Some(frame) = parse_frame(&header) else {
            break;
        };
        // 途中で別の音源に変わっていたら、そこはもう次のファイル。
        if let Some(f) = first
            && (f.sample_rate != frame.sample_rate
                || f.version != frame.version
                || f.layer != frame.layer)
        {
            break;
        }
        if pos + frame.length > limit {
            break;
        }
        first.get_or_insert(frame);
        total_samples += u64::from(frame.samples);
        pos += frame.length;
        frames += 1;
    }

    let need = if tagged {
        MIN_TAGGED_FRAMES
    } else {
        MIN_BARE_FRAMES
    };
    if frames < need {
        return None;
    }

    // 末尾の ID3v1 タグ(128 バイト固定)。
    let mut end = pos;
    if r.matches(end, b"TAG") && end + 128 <= limit {
        end += 128;
    }

    let mut candidate = Candidate::exact(FileFormat::Mp3, "mp3", end - start);
    if let Some(f) = first
        && f.sample_rate > 0
    {
        candidate.metadata.duration_ms = Some(total_samples * 1000 / u64::from(f.sample_rate));
    }
    Some(candidate)
}

/// 先頭に ID3v2 タグがあれば、その全長を返す。
fn id3v2_size(r: &mut Reader<'_>, start: u64) -> Option<u64> {
    if !r.matches(start, b"ID3") {
        return None;
    }
    let flags = r.u8(start + 5)?;
    let raw = r.array::<4>(start + 6)?;
    // サイズは synchsafe integer(各バイトの最上位ビットを使わない 7 ビット)。
    if raw.iter().any(|b| *b & 0x80 != 0) {
        return None;
    }
    let size = raw
        .iter()
        .fold(0u64, |acc, b| (acc << 7) | u64::from(*b & 0x7F));
    // フッタ付き(flags bit4)なら 10 バイト増える。
    let footer = if flags & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// 4 バイトのフレームヘッダを解釈する。フレームでなければ `None`。
fn parse_frame(h: &[u8; 4]) -> Option<Frame> {
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    // 00 = MPEG2.5, 01 = 予約, 10 = MPEG2, 11 = MPEG1
    let version = match (h[1] >> 3) & 0x03 {
        0 => 3u8, // 2.5
        1 => return None,
        2 => 2,
        _ => 1,
    };
    // 01 = Layer3, 10 = Layer2, 11 = Layer1
    let layer = match (h[1] >> 1) & 0x03 {
        0 => return None,
        1 => 3u8,
        2 => 2,
        _ => 1,
    };
    let bitrate_index = (h[2] >> 4) as usize;
    let sample_index = ((h[2] >> 2) & 0x03) as usize;
    if bitrate_index == 0 || bitrate_index == 15 || sample_index == 3 {
        return None;
    }
    let padding = u64::from((h[2] >> 1) & 0x01);

    let table = if version == 1 {
        &BITRATES_V1_L3
    } else {
        &BITRATES_V2_L3
    };
    // Layer1/2 は表が別だが、カービングで拾いたいのは実質 Layer3 だけなので
    // Layer3 以外は扱わない(誤検出も減らせる)。
    if layer != 3 {
        return None;
    }
    let bitrate = table[bitrate_index] * 1000;
    let sample_rate = SAMPLE_RATES[usize::from(version - 1)][sample_index];
    if bitrate == 0 || sample_rate == 0 {
        return None;
    }

    // MPEG1 Layer3 は 1152 サンプル/フレーム、MPEG2/2.5 Layer3 は 576。
    let samples = if version == 1 { 1152 } else { 576 };
    let length = u64::from(samples / 8) * u64::from(bitrate) / u64::from(sample_rate) + padding;
    if length < 24 {
        return None;
    }
    Some(Frame {
        length,
        sample_rate,
        samples,
        version,
        layer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_mpeg1_layer3_frame() {
        // FF FB 90 00 = MPEG1 Layer3 128kbps 44.1kHz パディングなし。
        let f = parse_frame(&[0xFF, 0xFB, 0x90, 0x00]).unwrap();
        assert_eq!(f.sample_rate, 44100);
        assert_eq!(f.samples, 1152);
        assert_eq!(f.length, 417);
    }

    #[test]
    fn rejects_reserved_and_free_format_headers() {
        // 予約バージョン。
        assert_eq!(parse_frame(&[0xFF, 0xEB, 0x90, 0x00]), None);
        // ビットレート free (0) と bad (15)。
        assert_eq!(parse_frame(&[0xFF, 0xFB, 0x00, 0x00]), None);
        assert_eq!(parse_frame(&[0xFF, 0xFB, 0xF0, 0x00]), None);
        // サンプリング周波数 予約。
        assert_eq!(parse_frame(&[0xFF, 0xFB, 0x9C, 0x00]), None);
        // 同期していない。
        assert_eq!(parse_frame(&[0xFF, 0x0B, 0x90, 0x00]), None);
    }
}
