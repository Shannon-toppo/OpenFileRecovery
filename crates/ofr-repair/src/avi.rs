//! AVI の修復。
//!
//! AVI は RIFF の入れ子で、`hdrl`(ヘッダ)、`movi`(フレーム本体)、`idx1`
//! (インデックス)の 3 つでできている。4 形式の中では構造が一番単純で、
//! **本体さえ残っていれば、インデックスもヘッダの数値も実測から作り直せる**
//! (PLAN.md 5.6)。
//!
//! | 壊れ方 | 対処 |
//! |---|---|
//! | idx1 の欠損・破損 | `movi` を歩いてチャンクの位置と大きさを実測し、作り直す |
//! | RIFF / LIST サイズの破損 | 実測値で書き直す(録画中に電源が落ちた場合に必ず起きる) |
//! | avih / strh の値の破損 | フレーム数・寸法・バッファサイズを実測値と `strf` から埋め直す |
//! | 末尾の切断 | 途中で切れたチャンクを落として、そこまでを健全な AVI にする |
//! | `hdrl` の欠損 | 参照ファイルから移植する |
//!
//! 出力は「RIFF ヘッダ + hdrl + movi + idx1」の形に組み直す。`movi` の中身は
//! 入力からそのまま流し込むので、フレームのデータには一切触らない。

use std::fs::File;
use std::io::{BufWriter, Write};

use crate::Job;
use crate::error::{RepairError, Result};
use crate::report::{RepairStatus, Verification};
use crate::source::Source;

/// 先頭で RIFF ヘッダを探す範囲。
const HEAD_SEARCH: u64 = 1024 * 1024;

/// 歩くチャンク数の上限。壊れた長さで無限に回らないための歯止め。
const MAX_CHUNKS: usize = 4_000_000;

/// インデックスに載せる 1 チャンク。
#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: [u8; 4],
    /// `movi` の 4 文字識別子を 0 とした位置。AVI のインデックスはこの基準を使う。
    offset: u32,
    size: u32,
    keyframe: bool,
}

/// `hdrl` から読み取ったストリーム 1 本の情報。
#[derive(Debug, Clone)]
struct Stream {
    /// `vids` / `auds` など。
    kind: [u8; 4],
    /// `hdrl` の中での `strh` 本体の位置。値を書き直すのに使う。
    strh_at: usize,
    /// 1 標本のバイト数。0 なら「1 チャンク = 1 標本」。
    sample_size: u32,
    /// 映像の圧縮形式(`strf` の biCompression)。
    compression: Option<[u8; 4]>,
    /// `strf` から読めた寸法。
    size: Option<(u32, u32)>,
    /// 実測したチャンク数。
    chunks: u32,
    /// 実測した総バイト数。
    bytes: u64,
}

/// AVI を修復する。
pub(crate) fn repair(job: &mut Job<'_>) -> Result<()> {
    let mut status = RepairStatus::Intact;

    // ---- RIFF ヘッダ ----
    let riff_at = match find_riff(job.src) {
        Some(0) => 0,
        Some(at) => {
            job.report
                .fixed(format!("先頭に付いていた {at} バイトのごみを落とした"));
            status = RepairStatus::Repaired;
            at
        }
        None => {
            job.report
                .issue("RIFF ヘッダが見つからない。AVI として読める部分がない");
            job.report.status = RepairStatus::Failed;
            return Ok(());
        }
    };
    let declared_end = job
        .src
        .u32le(riff_at + 4)
        .map(|s| riff_at + 8 + u64::from(s))
        .unwrap_or(job.src.len());
    if declared_end > job.src.len() {
        job.report.fixed(format!(
            "RIFF のサイズが実際より {} バイト大きかったので実測値に直した",
            declared_end - job.src.len()
        ));
        status = RepairStatus::Repaired;
    }
    let file_end = declared_end.min(job.src.len());

    // ---- 最上位のチャンクを見る ----
    let top = walk_top(job.src, riff_at + 12, file_end);

    // ---- hdrl ----
    let mut hdrl = match top.hdrl {
        Some((at, len)) => job.src.read_vec(at, len as usize),
        None => match reference_hdrl(job)? {
            Some(bytes) => {
                job.report
                    .fixed("失われていた hdrl (ヘッダ) を参照ファイルから移植した");
                job.report.issue(
                    "ヘッダは参照ファイルのものなので、元の解像度やフレームレートとは違う可能性がある",
                );
                status = RepairStatus::Repaired;
                bytes
            }
            None => {
                job.report.issue(
                    "hdrl (ヘッダ) が失われている。同じ機器で撮った正常なファイルを参照に指定すること",
                );
                job.report.status = RepairStatus::Failed;
                return Ok(());
            }
        },
    };

    // ---- movi ----
    let Some((movi_tag_at, movi_len)) = top.movi else {
        job.report.issue("movi (映像の本体) が見つからない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    };
    let movi_payload_at = movi_tag_at + 4;
    let declared_movi_end = movi_payload_at + u64::from(movi_len.saturating_sub(4));
    let mut streams = read_streams(&hdrl);
    let (entries, movi_end) = walk_movi(
        job.src,
        movi_tag_at,
        movi_payload_at,
        declared_movi_end.min(file_end),
        &mut streams,
    );

    if entries.is_empty() {
        job.report.issue("movi の中にフレームが 1 つも見つからない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    }
    if movi_end < declared_movi_end {
        job.report.fixed(format!(
            "途中で切れていた末尾 {} バイトを落とした",
            declared_movi_end - movi_end
        ));
        status = RepairStatus::Partial;
    }

    // ---- idx1 ----
    let index_ok =
        matches!(top.idx1, Some((_, len)) if u64::from(entries.len() as u32) * 16 == len);
    if !index_ok {
        let what = if top.idx1.is_some() {
            "壊れていた idx1 (インデックス) を実測値で作り直した"
        } else {
            "失われていた idx1 (インデックス) を実測値から作り直した"
        };
        job.report
            .fixed(format!("{what}: {} チャンク", entries.len()));
        status = status.or_repaired();
    }
    if entries.iter().any(|e| !e.keyframe) {
        // 何も分からない状態でキーフレーム扱いにするのは嘘になるので、
        // 圧縮形式から確実に言える場合だけ立てている。
        job.report.issue(
            "圧縮形式からキーフレームを判定できないフレームがある。\
             再生はできるが、シークが目的の位置からずれることがある",
        );
    }

    // ---- avih / strh を実測値で直す ----
    let fixes = patch_headers(&mut hdrl, &streams, &entries);
    for f in fixes {
        job.report.fixed(f);
        status = status.or_repaired();
    }

    // ---- 書き出し ----
    if status == RepairStatus::Intact && !job.options.write_intact {
        job.report.status = status;
        job.report.verification =
            Verification::Skipped("元から壊れていないので書き出していない".to_string());
        return Ok(());
    }
    let movi_payload_len = movi_end - movi_payload_at;
    let written = write_avi(job, &hdrl, movi_payload_at, movi_payload_len, &entries)?;

    job.report.output = Some(job.output.to_path_buf());
    job.report.output_size = written;
    job.report.status = status;
    if job.options.verify {
        job.report.verification = verify(job.output, entries.len());
    } else {
        job.report.verification = Verification::Skipped("検証を無効にしている".to_string());
    }
    Ok(())
}

/// 最上位で見つけたもの。
#[derive(Debug, Default)]
struct Top {
    /// `hdrl` の 4 文字識別子の位置と、LIST の長さ。
    hdrl: Option<(u64, u32)>,
    /// `movi` の 4 文字識別子の位置と、LIST の長さ。
    movi: Option<(u64, u32)>,
    /// `idx1` のデータ位置と長さ。
    idx1: Option<(u64, u64)>,
}

/// `RIFF` の位置を探す。
fn find_riff(src: &mut Source) -> Option<u64> {
    if src.matches(0, b"RIFF") && src.matches(8, b"AVI ") {
        return Some(0);
    }
    let limit = src.len().min(HEAD_SEARCH) as usize;
    let window = src.view(0, limit).to_vec();
    (0..window.len().saturating_sub(12))
        .find(|&i| &window[i..i + 4] == b"RIFF" && &window[i + 8..i + 12] == b"AVI ")
        .map(|i| i as u64)
}

/// 最上位のチャンクを歩く。
fn walk_top(src: &mut Source, from: u64, end: u64) -> Top {
    let mut top = Top::default();
    let mut pos = from;
    for _ in 0..1024 {
        if pos + 8 > end {
            break;
        }
        let (Some(tag), Some(size)) = (src.array::<4>(pos), src.u32le(pos + 4)) else {
            break;
        };
        if !is_tag(&tag) {
            break;
        }
        let body = pos + 8;
        match &tag {
            b"LIST" => match src.array::<4>(body) {
                Some(t) if &t == b"hdrl" => top.hdrl = Some((body, size)),
                Some(t) if &t == b"movi" => top.movi = Some((body, size)),
                _ => {}
            },
            b"idx1" => top.idx1 = Some((body, u64::from(size))),
            _ => {}
        }
        // チャンクは 2 バイト境界に揃う。
        let advance = 8 + u64::from(size) + u64::from(size & 1);
        let Some(next) = pos.checked_add(advance) else {
            break;
        };
        if next > end {
            // 宣言サイズが末尾を越えている。最後のチャンクとして扱う。
            break;
        }
        pos = next;
    }
    top
}

/// `movi` の中を歩いて、フレームの位置と大きさを実測する。
///
/// 戻り値は (インデックスの元になる一覧, 綺麗に並んでいた最後の位置)。
fn walk_movi(
    src: &mut Source,
    movi_tag_at: u64,
    from: u64,
    end: u64,
    streams: &mut [Stream],
) -> (Vec<Entry>, u64) {
    let mut entries = Vec::new();
    let mut pos = from;
    let mut clean_end = from;

    while entries.len() < MAX_CHUNKS {
        if pos + 8 > end {
            break;
        }
        let (Some(tag), Some(size)) = (src.array::<4>(pos), src.u32le(pos + 4)) else {
            break;
        };
        let body = pos + 8;
        let size64 = u64::from(size);

        // `LIST rec ` はフレームをまとめる入れ物。中に降りる。
        if &tag == b"LIST" {
            if body + 4 <= end && src.array::<4>(body).is_some() {
                pos = body + 4;
                continue;
            }
            break;
        }
        if &tag == b"JUNK" {
            let advance = 8 + size64 + (size64 & 1);
            if body + size64 > end {
                break;
            }
            pos += advance;
            clean_end = pos.min(end);
            continue;
        }
        if !is_frame_tag(&tag) || body + size64 > end {
            break;
        }

        let stream =
            usize::from(tag[0].wrapping_sub(b'0')) * 10 + usize::from(tag[1].wrapping_sub(b'0'));
        let keyframe = streams
            .get(stream)
            .map(|s| s.all_keyframes())
            .unwrap_or(false);
        if let Some(s) = streams.get_mut(stream) {
            s.chunks += 1;
            s.bytes += size64;
        }
        entries.push(Entry {
            tag,
            offset: (pos - movi_tag_at) as u32,
            size,
            keyframe,
        });

        pos = body + size64 + (size64 & 1);
        clean_end = pos.min(end);
    }

    (entries, clean_end)
}

/// 4 バイトが RIFF の識別子として妥当か。
fn is_tag(tag: &[u8; 4]) -> bool {
    tag.iter().all(|b| (0x20..=0x7E).contains(b))
}

/// `00dc` のようなフレームチャンクの識別子か。
fn is_frame_tag(tag: &[u8; 4]) -> bool {
    tag[0].is_ascii_digit()
        && tag[1].is_ascii_digit()
        && tag[2].is_ascii_alphanumeric()
        && tag[3].is_ascii_alphanumeric()
}

impl Stream {
    /// 全フレームがキーフレームだと確実に言える圧縮形式か。
    fn all_keyframes(&self) -> bool {
        if &self.kind != b"vids" {
            // 音声チャンクにキーフレーム標識を立てるのは慣例。
            return true;
        }
        match self.compression {
            // 無圧縮。
            Some([0, 0, 0, 0]) => true,
            Some(c) => matches!(
                &c,
                b"DIB " | b"RGB " | b"MJPG" | b"mjpg" | b"jpeg" | b"dvsd" | b"DVSD" | b"HFYU"
            ),
            None => false,
        }
    }
}

/// `hdrl` からストリーム情報を読む。
fn read_streams(hdrl: &[u8]) -> Vec<Stream> {
    let mut out = Vec::new();
    // "hdrl" の 4 バイトを飛ばして、中のチャンクを歩く。
    let mut pos = 4usize;
    while pos + 8 <= hdrl.len() {
        let tag: [u8; 4] = hdrl[pos..pos + 4].try_into().unwrap();
        let size = u32::from_le_bytes(hdrl[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if &tag == b"LIST" {
            // strl の中に降りる。
            pos = body + 4;
            continue;
        }
        if body + size > hdrl.len() {
            break;
        }
        match &tag {
            b"strh" if size >= 48 => out.push(Stream {
                kind: hdrl[body..body + 4].try_into().unwrap(),
                strh_at: body,
                sample_size: le32(hdrl, body + 44),
                compression: None,
                size: None,
                chunks: 0,
                bytes: 0,
            }),
            b"strf" if size >= 20 => {
                if let Some(s) = out.last_mut()
                    && &s.kind == b"vids"
                {
                    s.size = Some((le32(hdrl, body + 4), le32(hdrl, body + 8)));
                    s.compression = Some(hdrl[body + 16..body + 20].try_into().unwrap());
                }
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    out
}

fn le32(data: &[u8], at: usize) -> u32 {
    match data.get(at..at + 4) {
        Some(b) => u32::from_le_bytes(b.try_into().unwrap()),
        None => 0,
    }
}

fn put32(data: &mut [u8], at: usize, value: u32) -> bool {
    match data.get_mut(at..at + 4) {
        Some(slot) => {
            let changed = slot != value.to_le_bytes();
            slot.copy_from_slice(&value.to_le_bytes());
            changed
        }
        None => false,
    }
}

/// `avih` と `strh` の数値を実測値で埋め直す。直した項目の説明を返す。
fn patch_headers(hdrl: &mut [u8], streams: &[Stream], entries: &[Entry]) -> Vec<String> {
    let mut fixes = Vec::new();

    // avih は hdrl の直後に来る。
    if hdrl.len() >= 12 + 56 && &hdrl[4..8] == b"avih" {
        let avih = 12;
        let video = streams.iter().find(|s| &s.kind == b"vids");
        let frames = video.map(|s| s.chunks).unwrap_or(entries.len() as u32);
        let biggest = entries.iter().map(|e| e.size).max().unwrap_or(0);

        if put32(hdrl, avih + 16, frames) {
            fixes.push(format!("avih の総フレーム数を実測値 {frames} に直した"));
        }
        if put32(hdrl, avih + 24, streams.len() as u32) {
            fixes.push(format!("avih のストリーム数を {} に直した", streams.len()));
        }
        if le32(hdrl, avih + 28) < biggest {
            put32(hdrl, avih + 28, biggest);
            fixes.push("avih の推奨バッファサイズを最大チャンク長に合わせた".to_string());
        }
        // 寸法が 0 なら strf (BITMAPINFOHEADER) から埋める。
        if let Some((w, h)) = video.and_then(|s| s.size)
            && (le32(hdrl, avih + 32) == 0 || le32(hdrl, avih + 36) == 0)
        {
            put32(hdrl, avih + 32, w);
            put32(hdrl, avih + 36, h);
            fixes.push(format!("avih の寸法を strf から {w}x{h} に直した"));
        }
        // インデックスを必ず書くので AVIF_HASINDEX を立てる。
        let flags = le32(hdrl, avih + 12);
        if flags & 0x10 == 0 {
            put32(hdrl, avih + 12, flags | 0x10);
            fixes.push("avih にインデックスあり (AVIF_HASINDEX) の印を立てた".to_string());
        }
    }

    for s in streams {
        // dwSampleSize が 0 なら「1 チャンク = 1 標本」、それ以外は総バイト数から。
        let length = if s.sample_size == 0 {
            s.chunks
        } else {
            (s.bytes / u64::from(s.sample_size)) as u32
        };
        if put32(hdrl, s.strh_at + 32, length) {
            let kind = String::from_utf8_lossy(&s.kind).to_string();
            fixes.push(format!("strh ({kind}) の長さを実測値 {length} に直した"));
        }
    }

    fixes
}

/// 組み直した AVI を書き出す。戻り値は書いたバイト数。
fn write_avi(
    job: &mut Job<'_>,
    hdrl: &[u8],
    movi_payload_at: u64,
    movi_payload_len: u64,
    entries: &[Entry],
) -> Result<u64> {
    let file = File::create(job.output).map_err(|e| RepairError::output(job.output, e))?;
    let mut out = BufWriter::new(file);

    let hdrl_size = hdrl.len() as u64;
    let movi_size = 4 + movi_payload_len;
    let idx1_size = entries.len() as u64 * 16;
    let riff_size = 4 + (8 + hdrl_size) + (8 + movi_size) + (8 + idx1_size);

    let write = |out: &mut BufWriter<File>, buf: &[u8]| -> Result<()> {
        out.write_all(buf)
            .map_err(|e| RepairError::output(job.output, e))
    };

    write(&mut out, b"RIFF")?;
    write(&mut out, &(riff_size as u32).to_le_bytes())?;
    write(&mut out, b"AVI ")?;

    write(&mut out, b"LIST")?;
    write(&mut out, &(hdrl_size as u32).to_le_bytes())?;
    write(&mut out, hdrl)?;

    write(&mut out, b"LIST")?;
    write(&mut out, &(movi_size as u32).to_le_bytes())?;
    write(&mut out, b"movi")?;
    let copied = job
        .src
        .copy_range(&mut out, movi_payload_at, movi_payload_len)
        .map_err(|e| RepairError::output(job.output, e))?;
    if copied < movi_payload_len {
        // 読めなくなった分は 0 で埋める。ここで長さがずれるとインデックスが全部狂う。
        let missing = movi_payload_len - copied;
        job.report.issue(format!(
            "movi の {missing} バイトを読めなかったので 0 で埋めた"
        ));
        let zeros = vec![0u8; 64 * 1024];
        let mut left = missing;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            write(&mut out, &zeros[..n])?;
            left -= n as u64;
        }
    }

    write(&mut out, b"idx1")?;
    write(&mut out, &(idx1_size as u32).to_le_bytes())?;
    for e in entries {
        write(&mut out, &e.tag)?;
        write(
            &mut out,
            &if e.keyframe { 0x10u32 } else { 0 }.to_le_bytes(),
        )?;
        write(&mut out, &e.offset.to_le_bytes())?;
        write(&mut out, &e.size.to_le_bytes())?;
    }

    out.flush()
        .map_err(|e| RepairError::output(job.output, e))?;
    Ok(8 + riff_size)
}

/// 参照ファイルから `hdrl` を借りる。
fn reference_hdrl(job: &mut Job<'_>) -> Result<Option<Vec<u8>>> {
    let Some(path) = job.reference else {
        return Ok(None);
    };
    let mut src = Source::open(path)?;
    let Some(riff_at) = find_riff(&mut src) else {
        return Err(RepairError::reference(
            path,
            "AVI として読めない (参照ファイルは正常なものを指定すること)",
        ));
    };
    let end = src.len();
    let top = walk_top(&mut src, riff_at + 12, end);
    match top.hdrl {
        Some((at, len)) => Ok(Some(src.read_vec(at, len as usize))),
        None => Err(RepairError::reference(path, "hdrl が見つからない")),
    }
}

/// 書き出した AVI を読み直して、インデックスが実際のチャンクを指しているか確かめる。
///
/// 動画の自動検証はここまで(PLAN.md 5.6)。実際に再生できるかは人間が確かめる。
fn verify(path: &std::path::Path, expected: usize) -> Verification {
    let mut src = match Source::open(path) {
        Ok(s) => s,
        Err(e) => return Verification::Failed(format!("書き出したファイルを開けない: {e}")),
    };
    let Some(riff_at) = find_riff(&mut src) else {
        return Verification::Failed("RIFF ヘッダが無い".to_string());
    };
    let end = src.len();
    let top = walk_top(&mut src, riff_at + 12, end);
    let (Some((movi_at, _)), Some((idx1_at, idx1_len))) = (top.movi, top.idx1) else {
        return Verification::Failed("movi または idx1 が無い".to_string());
    };
    let count = (idx1_len / 16) as usize;
    if count != expected {
        return Verification::Failed(format!(
            "インデックスの件数が合わない (期待 {expected}, 実際 {count})"
        ));
    }
    for i in 0..count {
        let at = idx1_at + i as u64 * 16;
        let (Some(tag), Some(offset), Some(size)) =
            (src.array::<4>(at), src.u32le(at + 8), src.u32le(at + 12))
        else {
            return Verification::Failed(format!("{i} 番目のインデックスを読めない"));
        };
        let chunk = movi_at + u64::from(offset);
        if src.array::<4>(chunk) != Some(tag) || src.u32le(chunk + 4) != Some(size) {
            return Verification::Failed(format!(
                "{i} 番目のインデックスが指す位置にチャンクが無い"
            ));
        }
        if chunk + 8 + u64::from(size) > end {
            return Verification::Failed(format!("{i} 番目のチャンクがファイル末尾を越えている"));
        }
    }
    Verification::Container
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_tags_are_recognised() {
        assert!(is_frame_tag(b"00dc"));
        assert!(is_frame_tag(b"01wb"));
        assert!(!is_frame_tag(b"LIST"));
        assert!(!is_frame_tag(b"idx1"));
    }

    #[test]
    fn keyframes_are_only_claimed_for_known_codecs() {
        let mut s = Stream {
            kind: *b"vids",
            strh_at: 0,
            sample_size: 0,
            compression: Some(*b"MJPG"),
            size: None,
            chunks: 0,
            bytes: 0,
        };
        assert!(s.all_keyframes());
        s.compression = Some(*b"H264");
        assert!(!s.all_keyframes());
        s.compression = None;
        assert!(!s.all_keyframes());
        s.kind = *b"auds";
        assert!(s.all_keyframes());
    }

    #[test]
    fn patching_reports_only_actual_changes() {
        // hdrl + avih (56 バイト) の最小構成。
        let mut hdrl = Vec::new();
        hdrl.extend_from_slice(b"hdrl");
        hdrl.extend_from_slice(b"avih");
        hdrl.extend_from_slice(&56u32.to_le_bytes());
        hdrl.extend_from_slice(&[0u8; 56]);

        let entries = vec![
            Entry {
                tag: *b"00dc",
                offset: 4,
                size: 100,
                keyframe: true,
            },
            Entry {
                tag: *b"00dc",
                offset: 116,
                size: 100,
                keyframe: true,
            },
        ];
        let fixes = patch_headers(&mut hdrl, &[], &entries);
        assert!(
            fixes.iter().any(|f| f.contains("総フレーム数")),
            "{fixes:?}"
        );
        assert_eq!(le32(&hdrl, 12 + 16), 2);
        assert_eq!(le32(&hdrl, 12 + 28), 100);
        assert_eq!(le32(&hdrl, 12 + 12) & 0x10, 0x10);

        // 2 回目は直す所が無い。
        assert!(patch_headers(&mut hdrl, &[], &entries).is_empty());
    }
}
