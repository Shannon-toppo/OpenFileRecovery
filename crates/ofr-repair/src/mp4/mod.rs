//! MP4 / MOV の修復。
//!
//! この形式で一番多い致命傷は `moov` の欠損(PLAN.md 5.6)。`moov` は
//! 「どのフレームがファイルのどこにあるか」の索引で、多くの機器は録画終了時に
//! これを最後に書く。だから録画中に電源が落ちたりカードを抜いたりすると、
//! 実データ(`mdat`)だけが残って索引が無いファイルができる。プレイヤーから見ると
//! 中身が空のファイルと区別が付かない。
//!
//! | 壊れ方 | 対処 |
//! |---|---|
//! | `moov` の欠損 | 参照ファイルの `moov` を雛形に、`mdat` を走査して索引を作り直す |
//! | 末尾の切断 | 実データの外を指すサンプルを落として索引を作り直す |
//! | `mdat` のサイズ破損 | 実測値に直す |
//! | 末尾のごみ | 落とす |
//!
//! 索引の作り直しは、`mdat` の中の NAL ユニット境界を辿ってフレームの位置と
//! 大きさを実測し、コーデック設定(`stsd`)と時間軸だけを参照ファイルから借りる、
//! という手順で行う([`nal`])。参照ファイルは**同じ機器・同じ設定で録画した
//! 正常なファイル**でなければならない。解像度やコーデックが違うと、索引は
//! できても再生できないものになる。
//!
//! 参照ファイルが無い場合は H.264 / H.265 のエレメンタリストリーム(Annex-B)
//! として取り出すところまでしかできない。これは実装の手抜きではなく、
//! コーデック設定が本当にどこにも残っていないため(PLAN.md 10章)。

mod boxes;
mod nal;
mod track;

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::Job;
use crate::error::{RepairError, Result};
use crate::report::{RepairStatus, Verification};
use crate::source::Source;

use track::{Sample, Track};

/// `moov` としてメモリに載せる上限。索引だけなのでこれを超えることはまず無い。
const MAX_MOOV: u64 = 256 * 1024 * 1024;

/// 動画全体の時間の目盛り(ミリ秒)。
const MOVIE_TIMESCALE: u32 = 1000;

/// 参照ファイルが無いときに書く `ftyp`。
const DEFAULT_FTYP: &[u8] = b"isom\0\0\x02\0isomiso2avc1mp41";

/// MP4 / MOV を修復する。
pub(crate) fn repair(job: &mut Job<'_>) -> Result<()> {
    let layout = scan_layout(job.src);
    let mut status = RepairStatus::Intact;

    let Some(mdat) = layout.mdat else {
        job.report.issue("mdat (映像の本体) が見つからない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    };
    if layout.fragmented {
        job.report
            .issue("分割 (fragmented) MP4 の moof / traf には対応していない。読めた範囲だけを扱う");
    }
    if mdat.declared_end != mdat.end {
        job.report.fixed(format!(
            "mdat のサイズが実際と {} バイトずれていたので実測値に直した",
            mdat.declared_end.abs_diff(mdat.end)
        ));
        status = RepairStatus::Repaired;
    }

    // ---- トラックを用意する ----
    let mut tracks = read_tracks(job, &layout);
    let rebuilt_index = tracks.is_empty();
    if rebuilt_index {
        match rebuild_from_reference(job, &mdat)? {
            Some(t) => {
                tracks = t;
                status = RepairStatus::Repaired;
            }
            None => return rescue_elementary_stream(job, &mdat),
        }
    }

    // ---- 実データの外を指すサンプルを落とす ----
    let mut dropped = 0usize;
    for t in &mut tracks {
        let before = t.samples.len();
        t.samples
            .retain(|s| s.offset >= mdat.start && s.offset + u64::from(s.size) <= mdat.end);
        dropped += before - t.samples.len();
    }
    tracks.retain(|t| !t.samples.is_empty());
    if dropped > 0 {
        job.report.fixed(format!(
            "実データの外を指していたサンプル {dropped} 件を索引から外した"
        ));
        job.report
            .issue("末尾が失われている。索引から外した分のフレームは再生できない");
        status = RepairStatus::Partial;
    }
    if tracks.is_empty() {
        job.report
            .issue("実データの中に収まるサンプルが 1 つも無い");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    }

    // ---- 手を入れる必要が無ければ、そのまま写す ----
    if status == RepairStatus::Intact && layout.clean_end == job.src.len() && !rebuilt_index {
        return copy_verbatim(job);
    }
    if layout.clean_end < job.src.len() {
        job.report.fixed(format!(
            "末尾に付いていた {} バイトのごみを落とした",
            job.src.len() - layout.clean_end
        ));
        status = status.or_repaired();
    }

    // ---- 書き出し ----
    let ftyp = layout
        .ftyp
        .map(|(at, len)| job.src.read_vec(at, len as usize))
        .unwrap_or_else(|| boxes::make(b"ftyp", DEFAULT_FTYP));
    let written = write_mp4(job, &ftyp, &mut tracks, &mdat)?;

    job.report.output = Some(job.output.to_path_buf());
    job.report.output_size = written;
    job.report.status = status;
    job.report.verification = if job.options.verify {
        verify(job.output)
    } else {
        Verification::Skipped("検証を無効にしている".to_string())
    };
    Ok(())
}

/// ファイル先頭のボックス配置。
#[derive(Debug, Default)]
struct Layout {
    /// `ftyp` ボックス全体の位置と長さ。
    ftyp: Option<(u64, u64)>,
    /// `moov` の中身の位置と長さ。
    moov: Option<(u64, u64)>,
    /// `mdat` の中身。
    mdat: Option<Mdat>,
    /// ボックスを綺麗に辿れた最後の位置。
    clean_end: u64,
    /// 分割 MP4 の `moof` があったか。
    fragmented: bool,
}

/// `mdat`(実データ)の範囲。
#[derive(Debug, Clone, Copy)]
struct Mdat {
    /// 中身の開始位置。
    start: u64,
    /// 中身の終わり(実測)。
    end: u64,
    /// サイズ欄が主張していた終わり。
    declared_end: u64,
}

impl Mdat {
    fn len(&self) -> u64 {
        self.end - self.start
    }
}

/// 最上位のボックスを歩く。
fn scan_layout(src: &mut Source) -> Layout {
    let mut layout = Layout::default();
    let end = src.len();
    let mut pos = 0u64;

    for _ in 0..4096 {
        if pos + 8 > end {
            break;
        }
        let Some(size32) = src.u32be(pos) else { break };
        let tag: [u8; 4] = match src.array::<4>(pos + 4) {
            Some(t) if t.iter().all(|b| (0x20..=0x7E).contains(b)) => t,
            _ => break,
        };
        let (header, declared) = match size32 {
            // 0 は「ファイル末尾まで」。録画が途中で終わった MP4 でよく見る。
            0 => (8u64, end - pos),
            1 => match src.u64be(pos + 8) {
                Some(big) => (16u64, big),
                None => break,
            },
            n => (8u64, u64::from(n)),
        };
        if declared < header {
            break;
        }
        let body = pos + header;
        let declared_end = pos.saturating_add(declared);

        match &tag {
            b"ftyp" => layout.ftyp = Some((pos, declared.min(end - pos))),
            b"moov" => layout.moov = Some((body, declared_end.min(end).saturating_sub(body))),
            b"mdat" => {
                layout.mdat = Some(Mdat {
                    start: body,
                    end: declared_end.min(end),
                    declared_end,
                })
            }
            b"moof" => layout.fragmented = true,
            _ => {}
        }

        if declared_end > end {
            // 宣言サイズが末尾を越えている = ここで切れている。
            layout.clean_end = end;
            break;
        }
        pos = declared_end;
        layout.clean_end = pos;
    }

    layout
}

/// 入力の `moov` からトラックを読む。読めなければ空。
fn read_tracks(job: &mut Job<'_>, layout: &Layout) -> Vec<Track> {
    let Some((at, len)) = layout.moov else {
        return Vec::new();
    };
    if len == 0 || len > MAX_MOOV {
        return Vec::new();
    }
    let moov = job.src.read_vec(at, len as usize);
    track::parse_tracks(&moov)
        .into_iter()
        .filter(|t| !t.samples.is_empty())
        .collect()
}

/// 参照ファイルの `moov` を雛形に、`mdat` を走査して索引を作り直す。
fn rebuild_from_reference(job: &mut Job<'_>, mdat: &Mdat) -> Result<Option<Vec<Track>>> {
    let Some(path) = job.reference else {
        job.report.issue(
            "moov (索引) が失われていて、参照ファイルも指定されていない。\
             同じ機器・同じ設定で録画した正常なファイルを参照に指定すること",
        );
        return Ok(None);
    };

    let mut src = Source::open(path)?;
    let layout = scan_layout(&mut src);
    let Some((at, len)) = layout.moov.filter(|(_, len)| *len > 0 && *len <= MAX_MOOV) else {
        return Err(RepairError::reference(
            path,
            "moov が読めない (参照ファイルは正常なものを指定すること)",
        ));
    };
    let moov = src.read_vec(at, len as usize);
    let tracks = track::parse_tracks(&moov);

    let Some(template) = tracks.iter().find(|t| t.is_video()) else {
        return Err(RepairError::reference(path, "映像トラックが見つからない"));
    };
    let Some((codec, length_size)) = nal::codec_from_stsd(&template.stsd) else {
        return Err(RepairError::reference(
            path,
            "H.264 / H.265 以外のコーデックには対応していない",
        ));
    };

    let units = nal::scan(job.src, mdat.start, mdat.end, codec, length_size);
    if units.is_empty() {
        job.report.issue(format!(
            "mdat の中に {} のフレーム境界が見つからない。参照ファイルと録画設定が違う可能性がある",
            codec.label()
        ));
        return Ok(None);
    }

    let duration = template.average_duration();
    let samples: Vec<Sample> = units
        .iter()
        .map(|u| Sample {
            offset: u.offset,
            size: u.size,
            duration,
            sync: u.sync,
        })
        .collect();
    let covered = samples.iter().map(|s| u64::from(s.size)).sum::<u64>();

    job.report.fixed(format!(
        "参照ファイルの設定を借りて、mdat から {} フレーム ({}) の索引を作り直した",
        samples.len(),
        codec.label()
    ));
    if covered * 100 < mdat.len() * 90 {
        job.report.issue(format!(
            "mdat の {}% しかフレームとして解釈できなかった。音声など他のトラックのデータが混ざっている可能性が高い",
            covered * 100 / mdat.len().max(1)
        ));
    }
    if tracks.len() > 1 {
        job.report.issue(
            "音声トラックは復元していない。MP4 の音声はフレームの区切りがデータの中に無く、\
             索引を失うと境界を割り出せないため",
        );
    }
    job.report.issue(
        "時間軸は参照ファイルの平均フレーム間隔から組み直したので、可変フレームレートの録画では再生速度がずれる",
    );

    let mut rebuilt = template.clone();
    rebuilt.id = 1;
    rebuilt.samples = samples;
    Ok(Some(vec![rebuilt]))
}

/// `moov` も参照ファイルも無い場合の限定的な救済。
///
/// コンテナは作れないので、映像を Annex-B のエレメンタリストリームとして
/// 取り出す(PLAN.md 5.6)。出力の拡張子は中身に合わせて付け替える。
fn rescue_elementary_stream(job: &mut Job<'_>, mdat: &Mdat) -> Result<()> {
    // 長さ接頭辞は 4 バイトが圧倒的に多い。コーデックは中身から見当を付ける。
    let candidates = [(nal::Codec::H264, "h264"), (nal::Codec::H265, "h265")];
    let found = candidates.into_iter().find_map(|(codec, ext)| {
        let units = nal::scan(job.src, mdat.start, mdat.end, codec, 4);
        // 数フレームだけ偶然一致することがあるので、それなりの数を求める。
        (units.len() >= 8).then_some((codec, ext, units.len()))
    });

    let Some((codec, ext, frames)) = found else {
        job.report
            .issue("映像として取り出せる形のデータも見つからない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    };

    let output: PathBuf = job.output.with_extension(ext);
    let file = File::create(&output).map_err(|e| RepairError::output(&output, e))?;
    let mut out = BufWriter::new(file);
    let written = nal::write_annex_b(job.src, &mut out, mdat.start, mdat.end, 4, codec)
        .and_then(|n| out.flush().map(|()| n))
        .map_err(|e| RepairError::output(&output, e))?;

    job.report.fixed(format!(
        "mdat から {} の映像 {frames} フレームを Annex-B 形式で取り出した",
        codec.label()
    ));
    job.report.issue(format!(
        "MP4 としては組み直せなかったので、拡張子を .{ext} にして書き出した。\
         音声は失われ、再生できるプレイヤーも限られる。\
         同じ機器で録画した正常なファイルを参照に指定すれば MP4 として直せる"
    ));
    job.report.output = Some(output);
    job.report.output_size = written;
    job.report.status = RepairStatus::Partial;
    job.report.verification =
        Verification::Skipped("エレメンタリストリームにはコンテナ整合性の概念が無い".to_string());
    Ok(())
}

/// 直す所が無かったので、そのまま写す。
fn copy_verbatim(job: &mut Job<'_>) -> Result<()> {
    if !job.options.write_intact {
        job.report.status = RepairStatus::Intact;
        job.report.verification =
            Verification::Skipped("元から壊れていないので書き出していない".to_string());
        return Ok(());
    }
    let file = File::create(job.output).map_err(|e| RepairError::output(job.output, e))?;
    let mut out = BufWriter::new(file);
    let len = job.src.len();
    let written = job
        .src
        .copy_range(&mut out, 0, len)
        .and_then(|n| out.flush().map(|()| n))
        .map_err(|e| RepairError::output(job.output, e))?;

    job.report.output = Some(job.output.to_path_buf());
    job.report.output_size = written;
    job.report.status = RepairStatus::Intact;
    job.report.verification = if job.options.verify {
        verify(job.output)
    } else {
        Verification::Skipped("検証を無効にしている".to_string())
    };
    Ok(())
}

/// 組み直した MP4 を書き出す。戻り値は書いたバイト数。
///
/// `ftyp` → `moov` → `mdat` の順に置く。`moov` を先頭側に置くと、
/// 頭から読むだけで再生を始められる(ストリーミング向けの並びでもある)。
fn write_mp4(job: &mut Job<'_>, ftyp: &[u8], tracks: &mut [Track], mdat: &Mdat) -> Result<u64> {
    let payload_len = mdat.len();
    // 4GiB を越える実データは 64 ビット長のボックスヘッダが要る。
    let mdat_header: u64 = if payload_len + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };

    // サンプル位置は書き出し後の位置に直す。moov の大きさは位置の値によらないので
    // (co64 は固定長)、一度組んで大きさを測り、そのぶんずらせば辻褄が合う。
    let probe = track::build_moov(tracks, MOVIE_TIMESCALE);
    let new_payload_start = ftyp.len() as u64 + probe.len() as u64 + mdat_header;
    let shift = new_payload_start as i64 - mdat.start as i64;
    for t in tracks.iter_mut() {
        for s in &mut t.samples {
            s.offset = s.offset.saturating_add_signed(shift);
        }
    }
    let moov = track::build_moov(tracks, MOVIE_TIMESCALE);
    debug_assert_eq!(moov.len(), probe.len(), "moov の大きさが位置で変わっている");

    let file = File::create(job.output).map_err(|e| RepairError::output(job.output, e))?;
    let mut out = BufWriter::new(file);
    let output = job.output.to_path_buf();
    let write = |out: &mut BufWriter<File>, buf: &[u8]| -> Result<()> {
        out.write_all(buf)
            .map_err(|e| RepairError::output(&output, e))
    };

    write(&mut out, ftyp)?;
    write(&mut out, &moov)?;
    if mdat_header == 16 {
        write(&mut out, &1u32.to_be_bytes())?;
        write(&mut out, b"mdat")?;
        write(&mut out, &(payload_len + 16).to_be_bytes())?;
    } else {
        write(&mut out, &((payload_len + 8) as u32).to_be_bytes())?;
        write(&mut out, b"mdat")?;
    }
    let copied = job
        .src
        .copy_range(&mut out, mdat.start, payload_len)
        .map_err(|e| RepairError::output(job.output, e))?;
    if copied < payload_len {
        // 読めなかった分を詰めないと、以降のサンプル位置が全部ずれる。
        let missing = payload_len - copied;
        job.report.issue(format!(
            "mdat の {missing} バイトを読めなかったので 0 で埋めた"
        ));
        let zeros = vec![0u8; 64 * 1024];
        let mut left = missing;
        while left > 0 {
            let n = left.min(zeros.len() as u64) as usize;
            write(&mut out, &zeros[..n])?;
            left -= n as u64;
        }
    }
    out.flush()
        .map_err(|e| RepairError::output(job.output, e))?;

    Ok(ftyp.len() as u64 + moov.len() as u64 + mdat_header + payload_len)
}

/// 書き出した MP4 を読み直して、索引が実データの中を指しているか確かめる。
///
/// 動画の自動検証はここまで(PLAN.md 5.6)。再生できるかは人間が確かめる。
fn verify(path: &std::path::Path) -> Verification {
    let mut src = match Source::open(path) {
        Ok(s) => s,
        Err(e) => return Verification::Failed(format!("書き出したファイルを開けない: {e}")),
    };
    let layout = scan_layout(&mut src);
    if layout.clean_end != src.len() {
        return Verification::Failed(format!(
            "ボックスの並びが {} バイト目で途切れている",
            layout.clean_end
        ));
    }
    let (Some((moov_at, moov_len)), Some(mdat)) = (layout.moov, layout.mdat) else {
        return Verification::Failed("moov または mdat が無い".to_string());
    };
    let moov = src.read_vec(moov_at, moov_len as usize);
    let tracks = track::parse_tracks(&moov);
    if tracks.is_empty() {
        return Verification::Failed("トラックを読み取れない".to_string());
    }
    for t in &tracks {
        if t.samples.is_empty() {
            return Verification::Failed(format!("トラック {} にサンプルが無い", t.id));
        }
        for (i, s) in t.samples.iter().enumerate() {
            if s.offset < mdat.start || s.offset + u64::from(s.size) > mdat.end {
                return Verification::Failed(format!(
                    "トラック {} の {i} 番目のサンプルが mdat の外を指している",
                    t.id
                ));
            }
        }
    }
    Verification::Container
}
