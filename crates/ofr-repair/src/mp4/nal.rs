//! `mdat` の中を走査してフレームの境界を見つける。
//!
//! MP4 の中の H.264 / H.265 は「長さ + NAL ユニット」の繰り返しで並んでいる
//! (放送や `.h264` ファイルで使う開始コード `00 00 00 01` は付かない)。
//! この長さの連鎖を辿れば、`moov` が無くてもフレームの位置と大きさは復元できる。
//! これが参照ファイル方式による `moov` 再構築の中身
//! (PLAN.md 5.6「mdat 内の NAL ユニット境界を走査してサンプルテーブルを再構築する」)。
//!
//! 1 フレーム(アクセスユニット)は複数の NAL ユニットからなる。どこで切れるかは
//! スライスヘッダの先頭ビット(「この NAL が画面の先頭マクロブロックから始まるか」)と、
//! パラメータセット類の出現で判定する。ビットストリームの中身までは読まない。

use crate::source::Source;

/// 拾うフレーム数の上限。
const MAX_UNITS: usize = 8_000_000;

/// 対応する映像コーデック。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    /// H.264 / AVC。
    H264,
    /// H.265 / HEVC。
    H265,
}

impl Codec {
    /// 画面表示用の名前。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::H265 => "H.265",
        }
    }
}

/// `stsd` からコーデックと長さ接頭辞の大きさを読む。
///
/// `stsd` の入れ子を厳密に辿らず `avcC` / `hvcC` を直接探しているのは、
/// サンプルエントリの固定長部分の解釈がコーデックごとに微妙に違い、
/// 壊れたファイル相手には「探した方が確実」だから。
pub(crate) fn codec_from_stsd(stsd: &[u8]) -> Option<(Codec, u8)> {
    if let Some(at) = find(stsd, b"avcC") {
        // avcC: 版(1) プロファイル(1) 互換(1) レベル(1) lengthSizeMinusOne(1)
        let length_size = (stsd.get(at + 4 + 4)? & 0x03) + 1;
        return Some((Codec::H264, length_size));
    }
    if let Some(at) = find(stsd, b"hvcC") {
        // hvcC は固定部が長く、lengthSizeMinusOne は本体先頭から 21 バイト目。
        let length_size = (stsd.get(at + 4 + 21)? & 0x03) + 1;
        return Some((Codec::H265, length_size));
    }
    None
}

fn find(haystack: &[u8], needle: &[u8; 4]) -> Option<usize> {
    haystack.windows(4).position(|w| w == needle)
}

/// 見つけたアクセスユニット(= 1 フレーム)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AccessUnit {
    /// ファイル先頭からの位置。
    pub offset: u64,
    /// バイト数。
    pub size: u32,
    /// キーフレーム(IDR)か。
    pub sync: bool,
}

/// NAL ユニット 1 つの素性。
struct Nal {
    /// 映像そのもの(スライス)を運ぶ NAL か。
    vcl: bool,
    /// 新しいフレームの先頭になりうるか。
    starts_frame: bool,
    /// IDR(そこから再生を始められるフレーム)か。
    idr: bool,
}

/// `[start, end)` を走査してフレームの一覧を作る。
pub(crate) fn scan(
    src: &mut Source,
    start: u64,
    end: u64,
    codec: Codec,
    length_size: u8,
) -> Vec<AccessUnit> {
    let ls = u64::from(length_size);
    if !(1..=4).contains(&ls) {
        return Vec::new();
    }

    let mut units: Vec<AccessUnit> = Vec::new();
    let mut pos = start;
    let mut au_start = start;
    let mut au_has_vcl = false;
    let mut au_sync = false;

    while pos + ls <= end && units.len() < MAX_UNITS {
        let Some(len) = read_length(src, pos, length_size) else {
            break;
        };
        // 長さ 0 や、範囲をはみ出す長さはここで打ち切る。壊れているか、
        // そもそも長さ接頭辞形式ではない。
        if len == 0 || pos + ls + len > end {
            break;
        }
        let Some(nal) = classify(src, pos + ls, codec) else {
            break;
        };

        if nal.starts_frame && au_has_vcl {
            units.push(AccessUnit {
                offset: au_start,
                size: (pos - au_start) as u32,
                sync: au_sync,
            });
            au_start = pos;
            au_has_vcl = false;
            au_sync = false;
        }
        au_has_vcl |= nal.vcl;
        au_sync |= nal.idr;
        pos += ls + len;
    }

    if au_has_vcl && pos > au_start {
        units.push(AccessUnit {
            offset: au_start,
            size: (pos - au_start) as u32,
            sync: au_sync,
        });
    }
    units
}

fn read_length(src: &mut Source, at: u64, length_size: u8) -> Option<u64> {
    Some(match length_size {
        1 => u64::from(src.u8(at)?),
        2 => u64::from(u16::from_be_bytes(src.array::<2>(at)?)),
        3 => {
            let b = src.array::<3>(at)?;
            u64::from(u32::from_be_bytes([0, b[0], b[1], b[2]]))
        }
        _ => u64::from(src.u32be(at)?),
    })
}

/// NAL ヘッダを見て素性を判定する。読めない / 明らかに不正なら `None`。
fn classify(src: &mut Source, at: u64, codec: Codec) -> Option<Nal> {
    let first = src.u8(at)?;
    // 先頭ビット(forbidden_zero_bit)は必ず 0。ここが 1 なら NAL ではない。
    if first & 0x80 != 0 {
        return None;
    }
    match codec {
        Codec::H264 => {
            let kind = first & 0x1F;
            // 24 以降はストリーミング用の集約 NAL で、MP4 の中には現れない。
            if kind == 0 || kind > 23 {
                return None;
            }
            let vcl = matches!(kind, 1..=5);
            // スライスヘッダの最初の値 first_mb_in_slice が 0 (= 画面の先頭) なら
            // 新しいフレーム。ue(v) の 0 は先頭ビット 1 で表される。
            let first_mb_zero = src.u8(at + 1).is_some_and(|b| b & 0x80 != 0);
            Some(Nal {
                vcl,
                starts_frame: if vcl {
                    first_mb_zero
                } else {
                    // パラメータセット / SEI / アクセスユニット区切りは次のフレームの頭。
                    matches!(kind, 6..=9 | 13 | 15)
                },
                idr: kind == 5,
            })
        }
        Codec::H265 => {
            let kind = (first >> 1) & 0x3F;
            let second = src.u8(at + 1)?;
            // temporal_id_plus1 は 1 以上でなければならない。
            if second & 0x07 == 0 {
                return None;
            }
            let vcl = kind <= 31;
            let first_slice = src.u8(at + 2).is_some_and(|b| b & 0x80 != 0);
            Some(Nal {
                vcl,
                starts_frame: if vcl {
                    first_slice
                } else {
                    // VPS / SPS / PPS / AUD / SEI。
                    matches!(kind, 32..=35 | 39 | 40)
                },
                // BLA / IDR / CRA。
                idr: (16..=21).contains(&kind),
            })
        }
    }
}

/// フレームを Annex-B(開始コード付き)として書き出す。
///
/// `moov` も参照ファイルも無い場合の限定的な救済(PLAN.md 5.6)。コンテナは
/// 作れないが、映像そのものは取り出せる。書けたバイト数を返す。
pub(crate) fn write_annex_b(
    src: &mut Source,
    out: &mut dyn std::io::Write,
    start: u64,
    end: u64,
    length_size: u8,
    codec: Codec,
) -> std::io::Result<u64> {
    let ls = u64::from(length_size);
    let mut pos = start;
    let mut written = 0u64;
    while pos + ls <= end {
        let Some(len) = read_length(src, pos, length_size) else {
            break;
        };
        if len == 0 || pos + ls + len > end {
            break;
        }
        if classify(src, pos + ls, codec).is_none() {
            break;
        }
        out.write_all(&[0, 0, 0, 1])?;
        written += 4 + src.copy_range(out, pos + ls, len)?;
        pos += ls + len;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 長さ接頭辞付きの NAL を組み立てる。
    fn nal(kind: u8, first_mb_zero: bool, payload: usize) -> Vec<u8> {
        let mut body = vec![kind & 0x1F];
        body.push(if first_mb_zero { 0x88 } else { 0x08 });
        body.extend(std::iter::repeat_n(0x33, payload));
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend(body);
        out
    }

    fn source(data: &[u8]) -> (tempfile::NamedTempFile, Source) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        let src = Source::open(f.path()).unwrap();
        (f, src)
    }

    #[test]
    fn groups_nal_units_into_frames() {
        let mut data = Vec::new();
        // フレーム 1: SPS + PPS + IDR スライス。
        data.extend(nal(7, false, 10));
        data.extend(nal(8, false, 4));
        data.extend(nal(5, true, 100));
        // フレーム 2: 通常スライス。
        data.extend(nal(1, true, 80));
        // フレーム 3: 2 枚のスライスに分かれたフレーム。
        data.extend(nal(1, true, 40));
        data.extend(nal(1, false, 40));

        let len = data.len() as u64;
        let (_f, mut src) = source(&data);
        let units = scan(&mut src, 0, len, Codec::H264, 4);

        assert_eq!(units.len(), 3);
        assert!(units[0].sync, "IDR を含むフレームはキーフレーム");
        assert!(!units[1].sync);
        assert_eq!(units[0].offset, 0);
        assert_eq!(
            units.iter().map(|u| u64::from(u.size)).sum::<u64>(),
            len,
            "全バイトがどれかのフレームに入る"
        );
    }

    #[test]
    fn stops_on_data_that_is_not_length_prefixed() {
        let data = vec![0xFFu8; 4096];
        let (_f, mut src) = source(&data);
        assert!(scan(&mut src, 0, 4096, Codec::H264, 4).is_empty());
    }

    #[test]
    fn reads_the_length_prefix_size_from_avcc() {
        // stsd の中に avcC を置いただけの最小構成。
        let mut stsd = b"\0\0\0\x08stsdavc1".to_vec();
        stsd.extend_from_slice(b"\0\0\0\x0favcC");
        stsd.extend_from_slice(&[1, 0x64, 0, 0x1F, 0xFF]); // lengthSizeMinusOne = 3
        assert_eq!(codec_from_stsd(&stsd), Some((Codec::H264, 4)));
        assert_eq!(codec_from_stsd(b"nothing here"), None);
    }

    #[test]
    fn annex_b_output_has_start_codes() {
        let mut data = Vec::new();
        data.extend(nal(5, true, 20));
        data.extend(nal(1, true, 20));
        let len = data.len() as u64;
        let (_f, mut src) = source(&data);

        let mut out = Vec::new();
        let written = write_annex_b(&mut src, &mut out, 0, len, 4, Codec::H264).unwrap();
        assert_eq!(written as usize, out.len());
        assert_eq!(&out[..4], &[0, 0, 0, 1]);
        // 長さ接頭辞 4 バイトが開始コード 4 バイトに置き換わるので同じ長さ。
        assert_eq!(out.len() as u64, len);
    }
}
