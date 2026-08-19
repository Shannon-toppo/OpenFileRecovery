//! PNG の修復。
//!
//! PNG はチャンク(長さ + 型 + データ + CRC)の列でできていて、どのチャンクも
//! 自分の長さと CRC を持っている。壊れ方が局所的なら、この自己記述性のおかげで
//! かなり機械的に直せる(PLAN.md 5.6)。
//!
//! | 壊れ方 | 対処 |
//! |---|---|
//! | CRC 不一致 | 中身から計算し直して入れ替える |
//! | 長さフィールドの破損 | 次のチャンク型を探して長さを逆算する |
//! | シグネチャ / IEND の欠損 | 付け直す |
//! | IDAT の中身の破損・切断 | zlib をデコードできた行まで残し、以降を埋めて再圧縮する |
//!
//! CRC を計算し直すのは「壊れていないことにする」わけではない。PNG の CRC は
//! 検出用でしかなく訂正はできないので、直せるのは「中身は無事だが CRC だけずれた」
//! 場合に限る。中身の方が壊れていれば IDAT のデコードで露見する。

use crc32fast::Hasher;

use crate::Job;
use crate::error::{RepairError, Result};
use crate::report::RepairStatus;
use crate::source::Source;

/// PNG のシグネチャ。
const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// チャンク長の上限(仕様上 2^31-1)。
const MAX_CHUNK: u32 = 0x7FFF_FFFF;

/// 先頭のシグネチャ / IHDR を探す範囲。
const HEAD_SEARCH: usize = 64 * 1024;

/// フィルタ解除前の画像データとして扱う上限。
///
/// IHDR が壊れていると寸法が桁違いの値になる。素直に信じると、その大きさの
/// バッファを取ろうとしてメモリを食い潰す。8K RGBA でも 130MiB 程度なので、
/// これを超える値は壊れているとみなす。
const MAX_RAW: u64 = 512 * 1024 * 1024;

/// 再同期で探す既知のチャンク型。
const KNOWN_TAGS: [&[u8; 4]; 21] = [
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"tEXt",
    b"zTXt", b"iTXt", b"bKGD", b"pHYs", b"sBIT", b"sPLT", b"hIST", b"tIME", b"eXIf", b"acTL",
    b"fcTL",
];

/// 読み取れたチャンク 1 つ。
#[derive(Debug, Clone)]
struct Chunk {
    tag: [u8; 4],
    /// データ部の範囲。
    data: (usize, usize),
    /// CRC が合っていたか。
    crc_ok: bool,
    /// 長さフィールドを信じずに組み直したか。
    recovered_length: bool,
}

/// 画像ヘッダ(IHDR)の中身。
#[derive(Debug, Clone, Copy)]
struct Ihdr {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    interlace: u8,
}

impl Ihdr {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 13 {
            return None;
        }
        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ihdr = Ihdr {
            width,
            height,
            bit_depth: data[8],
            color_type: data[9],
            interlace: data[12],
        };
        if width == 0 || height == 0 || ihdr.channels().is_none() {
            return None;
        }
        Some(ihdr)
    }

    /// 1 画素あたりの標本数。色種別が不正なら `None`。
    fn channels(&self) -> Option<u32> {
        Some(match self.color_type {
            0 => 1, // グレースケール
            2 => 3, // トゥルーカラー
            3 => 1, // パレット
            4 => 2, // グレースケール + アルファ
            6 => 4, // トゥルーカラー + アルファ
            _ => return None,
        })
    }

    /// 1 行のバイト数(先頭のフィルタ種別 1 バイトを含む)。
    fn row_bytes(&self) -> Option<u64> {
        let channels = u64::from(self.channels()?);
        let bits = u64::from(self.width) * channels * u64::from(self.bit_depth);
        Some(1 + bits.div_ceil(8))
    }

    /// フィルタ解除前の画像データの総バイト数(非インターレースの場合)。
    fn raw_size(&self) -> Option<u64> {
        Some(self.row_bytes()? * u64::from(self.height))
    }

    /// 欠けた画素を埋める値。パレット画像は範囲外の番号を書けないので 0 にする。
    fn fill_byte(&self, fill: u8) -> u8 {
        if self.color_type == 3 { 0 } else { fill }
    }
}

/// PNG を修復する。
pub(crate) fn repair(job: &mut Job<'_>) -> Result<()> {
    let data = job.src.read_all(job.options.max_in_memory)?;
    let mut status = RepairStatus::Intact;
    let mut out = Vec::with_capacity(data.len() + 4096);
    out.extend_from_slice(SIGNATURE);

    // ---- シグネチャと走査開始位置 ----
    let start = match head_offset(&data) {
        Some(0) => 8,
        Some(at) => {
            job.report.fixed(format!(
                "先頭の {at} バイトを落としてシグネチャを付け直した"
            ));
            status = RepairStatus::Repaired;
            at + 8
        }
        None => match first_chunk_without_signature(&data) {
            Some(at) => {
                job.report.fixed("失われていたシグネチャを付け直した");
                status = RepairStatus::Repaired;
                at
            }
            None => {
                job.report.issue("PNG のチャンクが 1 つも見つからない");
                job.report.status = RepairStatus::Failed;
                return Ok(());
            }
        },
    };

    // ---- チャンク列 ----
    let chunks = walk(&data, start);
    let crc_errors = chunks.iter().filter(|c| !c.crc_ok).count();
    let recovered = chunks.iter().filter(|c| c.recovered_length).count();
    if crc_errors > 0 {
        job.report.fixed(format!(
            "CRC が合わないチャンク {crc_errors} 件を計算し直した"
        ));
        status = RepairStatus::Repaired;
    }
    if recovered > 0 {
        job.report.fixed(format!(
            "長さフィールドが壊れたチャンク {recovered} 件を、次のチャンクの位置から組み直した"
        ));
        status = RepairStatus::Repaired;
    }

    // ---- IHDR ----
    let ihdr_data = chunks
        .iter()
        .find(|c| &c.tag == b"IHDR")
        .map(|c| data[c.data.0..c.data.1].to_vec());
    let (ihdr_bytes, ihdr) = match ihdr_data
        .as_deref()
        .and_then(|d| Ihdr::parse(d).map(|h| (d, h)))
    {
        Some((d, h)) => (d.to_vec(), h),
        None => match reference_ihdr(job)? {
            Some((d, h)) => {
                job.report
                    .fixed("失われていた IHDR を参照ファイルから移植した");
                status = RepairStatus::Repaired;
                (d, h)
            }
            None => {
                job.report.issue(
                    "IHDR (寸法と色の形式) が失われている。同じ機器で撮った正常なファイルを参照に指定すること",
                );
                job.report.status = RepairStatus::Failed;
                return Ok(());
            }
        },
    };
    write_chunk(&mut out, b"IHDR", &ihdr_bytes);

    // ---- IDAT 以外のチャンクを順に写す ----
    for c in &chunks {
        if matches!(&c.tag, b"IHDR" | b"IDAT" | b"IEND") {
            continue;
        }
        write_chunk(&mut out, &c.tag, &data[c.data.0..c.data.1]);
    }

    // ---- 画像データ ----
    let idat: Vec<u8> = chunks
        .iter()
        .filter(|c| &c.tag == b"IDAT")
        .flat_map(|c| data[c.data.0..c.data.1].iter().copied())
        .collect();
    if idat.is_empty() {
        job.report.issue("IDAT (画像の中身) が残っていない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    }
    match rebuild_pixels(&idat, &ihdr, job.options.fill) {
        Pixels::Intact => write_chunk(&mut out, b"IDAT", &idat),
        Pixels::Rebuilt {
            data: rebuilt,
            recovered_rows,
            total_rows,
        } => {
            write_chunk(&mut out, b"IDAT", &rebuilt);
            let percent = recovered_rows * 100 / total_rows.max(1);
            job.report.fixed(format!(
                "IDAT を {recovered_rows}/{total_rows} 行 ({percent}%) まで復元し、残りを埋めて圧縮し直した"
            ));
            job.report.issue(format!(
                "画像の下側 {}% は元のデータが失われている",
                100 - percent
            ));
            status = RepairStatus::Partial;
        }
        Pixels::Undecodable(why) => {
            // インターレース画像など、行単位で組み直せないもの。中身はそのまま残す。
            write_chunk(&mut out, b"IDAT", &idat);
            job.report.issue(why);
            status = RepairStatus::Partial;
        }
    }

    // ---- IEND ----
    if !chunks.iter().any(|c| &c.tag == b"IEND") {
        job.report.fixed("失われていた IEND を付け直した");
        status = status.or_repaired();
    }
    write_chunk(&mut out, b"IEND", &[]);

    job.finish_image(&out, status)?;
    Ok(())
}

/// シグネチャの位置を探す。
fn head_offset(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(HEAD_SEARCH);
    data.get(..limit)?
        .windows(SIGNATURE.len())
        .position(|w| w == SIGNATURE)
}

/// シグネチャが無いファイルで、最初のチャンクの開始位置を探す。
fn first_chunk_without_signature(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(HEAD_SEARCH);
    let window = data.get(..limit)?;
    // 既知のチャンク型のうち、一番手前にあるものを起点にする。
    (0..window.len().saturating_sub(4))
        .find(|&i| {
            let tag: &[u8; 4] = window[i..i + 4].try_into().unwrap();
            KNOWN_TAGS.contains(&tag)
        })
        // 型の 4 バイト前が長さフィールド。
        .and_then(|i| i.checked_sub(4))
}

/// チャンク列を歩く。壊れた長さフィールドはここで組み直す。
fn walk(data: &[u8], start: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut pos = start;

    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        let tag: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let body = pos + 8;
        let plausible = len <= MAX_CHUNK && body + len as usize + 4 <= data.len();

        if !is_tag(&tag) {
            // 型が読めない = ここはもうチャンクの頭ではない。次を探す。
            match next_tag(data, pos + 1) {
                Some(next) => {
                    pos = next - 4;
                    continue;
                }
                None => break,
            }
        }

        let (end, recovered_length) = if plausible {
            (body + len as usize, false)
        } else {
            // 長さが信用できない。次のチャンク型の手前までを中身とみなす。
            match next_tag(data, body) {
                // 次の型の 4 バイト前が長さフィールド、その 4 バイト前が CRC。
                Some(next) if next >= body + 8 => (next - 8, true),
                _ => (data.len().saturating_sub(4).max(body), true),
            }
        };
        let end = end.min(data.len());

        let crc_ok = if recovered_length {
            false
        } else {
            match data.get(end..end + 4) {
                Some(stored) => {
                    let mut h = Hasher::new();
                    h.update(&tag);
                    h.update(&data[body..end]);
                    h.finalize().to_be_bytes() == stored
                }
                None => false,
            }
        };

        let is_end = &tag == b"IEND";
        chunks.push(Chunk {
            tag,
            data: (body, end),
            crc_ok,
            recovered_length,
        });
        if is_end {
            break;
        }
        pos = end + 4;
    }

    chunks
}

/// 4 バイトが ASCII の英字だけでできているか(チャンク型の条件)。
fn is_tag(tag: &[u8; 4]) -> bool {
    tag.iter().all(|b| b.is_ascii_alphabetic())
}

/// `from` 以降で最初に現れる既知のチャンク型の位置を返す。
fn next_tag(data: &[u8], from: usize) -> Option<usize> {
    if from + 4 > data.len() {
        return None;
    }
    (from..=data.len() - 4).find(|&i| {
        let tag: &[u8; 4] = data[i..i + 4].try_into().unwrap();
        KNOWN_TAGS.contains(&tag)
    })
}

/// チャンクを 1 つ書く(長さ・型・データ・CRC)。
fn write_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut h = Hasher::new();
    h.update(tag);
    h.update(data);
    out.extend_from_slice(&h.finalize().to_be_bytes());
}

/// IDAT の中身を調べた結果。
enum Pixels {
    /// そのまま使える。
    Intact,
    /// デコードできた所まで残して組み直した。
    Rebuilt {
        data: Vec<u8>,
        recovered_rows: u64,
        total_rows: u64,
    },
    /// 壊れているが、行単位では組み直せない。
    Undecodable(String),
}

/// zlib ストリームを、読める所まで展開する。
///
/// 途中で壊れていても、そこまでに展開できた分は正しい。一括展開の API は
/// 失敗すると出力を返してくれないので、ストリーム API を直接回している。
/// 戻り値は (展開できたバイト列, 最後まで読めたか)。
fn inflate_as_far_as_possible(input: &[u8], limit: usize) -> (Vec<u8>, bool) {
    use miniz_oxide::inflate::stream::{InflateState, inflate};
    use miniz_oxide::{DataFormat, MZFlush, MZStatus};

    let mut state = InflateState::new_boxed(DataFormat::Zlib);
    let mut out = vec![0u8; limit];
    let (mut written, mut consumed) = (0usize, 0usize);
    let mut complete = false;

    loop {
        if written >= out.len() {
            // 画像 1 枚分を取り切った。後ろに続きがあっても要らない。
            complete = true;
            break;
        }
        let r = inflate(
            &mut state,
            &input[consumed..],
            &mut out[written..],
            MZFlush::None,
        );
        consumed += r.bytes_consumed;
        written += r.bytes_written;
        match r.status {
            Ok(MZStatus::StreamEnd) => {
                complete = true;
                break;
            }
            // 進まなくなったら、そこが壊れている所(または入力の終わり)。
            Ok(_) if r.bytes_consumed == 0 && r.bytes_written == 0 => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    out.truncate(written);
    (out, complete)
}

/// IDAT の zlib ストリームを展開し、途中で切れていれば行単位で組み直す。
fn rebuild_pixels(idat: &[u8], ihdr: &Ihdr, fill: u8) -> Pixels {
    let (Some(row_bytes), Some(expected)) = (ihdr.row_bytes(), ihdr.raw_size()) else {
        return Pixels::Undecodable("色の形式が不正で、画像データを解釈できない".to_string());
    };
    if expected > MAX_RAW {
        return Pixels::Undecodable(format!(
            "IHDR の寸法 ({}x{}) から計算した画像データが大きすぎる。寸法が壊れている",
            ihdr.width, ihdr.height
        ));
    }
    let limit = expected as usize;

    // 展開できた分だけを取り出す。
    let (raw, complete) = inflate_as_far_as_possible(idat, limit);

    if complete && raw.len() as u64 >= expected {
        return Pixels::Intact;
    }
    if ihdr.interlace != 0 {
        return Pixels::Undecodable(
            "インターレース PNG の画像データが途中で切れている。行単位で組み直せないので中身はそのまま残した"
                .to_string(),
        );
    }

    // 完全に読めた行までを残し、以降を埋める。
    let recovered_rows = raw.len() as u64 / row_bytes;
    let total_rows = u64::from(ihdr.height);
    let keep = (recovered_rows * row_bytes) as usize;
    let mut rebuilt = raw[..keep].to_vec();
    let fill_byte = ihdr.fill_byte(fill);
    for _ in recovered_rows..total_rows {
        // フィルタ種別 0 (無変換) にしておけば、前の行に依存せず一色になる。
        rebuilt.push(0);
        rebuilt.extend(std::iter::repeat_n(fill_byte, (row_bytes - 1) as usize));
    }

    Pixels::Rebuilt {
        data: miniz_oxide::deflate::compress_to_vec_zlib(&rebuilt, 6),
        recovered_rows,
        total_rows,
    }
}

/// 参照ファイルから IHDR を借りる。
fn reference_ihdr(job: &mut Job<'_>) -> Result<Option<(Vec<u8>, Ihdr)>> {
    let Some(path) = job.reference else {
        return Ok(None);
    };
    let mut src = Source::open(path)?;
    let data = src.read_all(job.options.max_in_memory)?;
    let start = match head_offset(&data) {
        Some(at) => at + 8,
        None => {
            return Err(RepairError::reference(
                path,
                "PNG として読めない (参照ファイルは正常なものを指定すること)",
            ));
        }
    };
    let chunks = walk(&data, start);
    let ihdr = chunks
        .iter()
        .find(|c| &c.tag == b"IHDR")
        .map(|c| data[c.data.0..c.data.1].to_vec())
        .and_then(|d| Ihdr::parse(&d).map(|h| (d, h)));
    match ihdr {
        Some(v) => Ok(Some(v)),
        None => Err(RepairError::reference(path, "IHDR が読めない")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ihdr(color_type: u8, bit_depth: u8, w: u32, h: u32) -> Ihdr {
        Ihdr {
            width: w,
            height: h,
            bit_depth,
            color_type,
            interlace: 0,
        }
    }

    #[test]
    fn row_size_follows_the_colour_format() {
        // トゥルーカラー 8 ビット: 3 バイト/画素 + フィルタ 1 バイト。
        assert_eq!(ihdr(2, 8, 10, 5).row_bytes(), Some(31));
        // グレースケール 1 ビット: 10 ビット → 2 バイトに切り上げ。
        assert_eq!(ihdr(0, 1, 10, 5).row_bytes(), Some(3));
        assert_eq!(ihdr(2, 8, 10, 5).raw_size(), Some(155));
        assert_eq!(ihdr(9, 8, 10, 5).channels(), None);
    }

    #[test]
    fn palette_images_are_filled_with_index_zero() {
        assert_eq!(ihdr(3, 8, 4, 4).fill_byte(0x80), 0);
        assert_eq!(ihdr(2, 8, 4, 4).fill_byte(0x80), 0x80);
    }

    #[test]
    fn partial_zlib_streams_are_rebuilt_row_by_row() {
        let head = ihdr(2, 8, 4, 4); // 1 行 13 バイト、4 行
        let raw: Vec<u8> = (0..head.raw_size().unwrap() as usize)
            .map(|i| (i % 251) as u8)
            .collect();
        let full = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

        // 途中で切れた zlib ストリーム。
        let cut = &full[..full.len() / 2];
        match rebuild_pixels(cut, &head, 0x80) {
            Pixels::Rebuilt {
                data,
                recovered_rows,
                total_rows,
            } => {
                assert_eq!(total_rows, 4);
                assert!(recovered_rows < 4, "全部読めてしまっている");
                let back = miniz_oxide::inflate::decompress_to_vec_zlib(&data).unwrap();
                assert_eq!(back.len() as u64, head.raw_size().unwrap());
                // 埋めた行はフィルタ 0 + 埋め値。
                let row = (recovered_rows * head.row_bytes().unwrap()) as usize;
                assert_eq!(back[row], 0);
                assert_eq!(back[row + 1], 0x80);
            }
            _ => panic!("組み直されていない"),
        }

        assert!(matches!(rebuild_pixels(&full, &head, 0x80), Pixels::Intact));
    }

    #[test]
    fn walks_chunks_and_notices_bad_crc() {
        let mut data = Vec::new();
        data.extend_from_slice(SIGNATURE);
        write_chunk(&mut data, b"IHDR", &[0u8; 13]);
        write_chunk(&mut data, b"IDAT", b"body");
        write_chunk(&mut data, b"IEND", &[]);

        let chunks = walk(&data, 8);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.crc_ok));

        // CRC を 1 バイト壊す。
        let idat_crc = 8 + (12 + 13) + 8 + 4 + 3;
        data[idat_crc] ^= 0xFF;
        let chunks = walk(&data, 8);
        assert_eq!(chunks.iter().filter(|c| !c.crc_ok).count(), 1);
    }
}
