//! JPEG の修復。
//!
//! 復旧の現場で出てくる JPEG の壊れ方はだいたい 3 種類で、対処もそれぞれ違う。
//!
//! | 壊れ方 | 症状 | 対処 |
//! |---|---|---|
//! | ヘッダ(SOI〜SOS)の破損 | 全く開けない | 参照ファイル、または標準テーブルでヘッダを組み直す |
//! | 途中で切れている | 上半分だけ見える / 開けない | 残りをグレーで埋めて EOI を付ける([`fill`]) |
//! | 末尾にごみが付いている | 開けるが警告が出る | EOI の後ろを落とす |
//!
//! 出力は「ヘッダ + エントロピー符号 + (グレー埋め) + EOI」の形で組み立て直す。
//! 手を入れる必要が無かった部分は元のバイト列をそのまま写すので、直せた所以外は
//! 1 バイトも変わらない。

mod fill;
mod scan;
mod tables;

use crate::Job;
use crate::error::{RepairError, Result};
use crate::report::RepairStatus;
use crate::source::Source;

use scan::{Jpeg, Sof};

/// JPEG を修復する。
pub(crate) fn repair(job: &mut Job<'_>) -> Result<()> {
    let data = job.src.read_all(job.options.max_in_memory)?;
    let mut found = scan::scan(&data);
    if !found.header_is_usable() {
        // ヘッダが欠けている。順に辿るのを諦めて、生き残ったセグメントを拾い直す。
        found.fill_gaps_from(scan::scan_orphans(&data));
    }
    let reference = load_reference(job)?;

    let mut status = RepairStatus::Intact;
    let mut out = Vec::with_capacity(data.len() + 4096);

    // ---- ヘッダ ----
    let header = if found.header_is_usable() {
        let soi = found.soi.unwrap_or(0);
        if soi > 0 {
            job.report
                .fixed(format!("先頭に付いていた {soi} バイトのごみを落とした"));
            status = RepairStatus::Repaired;
        }
        let end = found.entropy_start().unwrap_or(soi);
        out.extend_from_slice(&data[soi..end]);
        HeaderInfo {
            sof: found.sof.clone(),
            sos: found.sos.clone(),
            huffman: found.huffman.clone(),
            dri: found.dri,
        }
    } else {
        status = RepairStatus::Repaired;
        rebuild_header(job, &data, &found, reference.as_ref(), &mut out)?
    };

    // ---- エントロピー符号 ----
    let entropy_start = found.entropy_start().unwrap_or(0);
    let entropy_end = found.eoi.unwrap_or(data.len());
    if entropy_start >= entropy_end {
        job.report
            .issue("エントロピー符号(画像の中身)が残っていない");
        job.report.status = RepairStatus::Failed;
        return Ok(());
    }

    // ---- 切れているなら残りをグレーで埋める ----
    let truncated = found.eoi.is_none();
    let mut cut = entropy_end;
    if truncated {
        status = RepairStatus::Partial;
        match gray_fill(&header, &found, entropy_start) {
            Some((tail, at)) => {
                cut = at;
                out.extend_from_slice(&data[entropy_start..cut]);
                out.extend_from_slice(&tail.data);
                job.report.fixed(format!(
                    "切れた末尾に EOI を付け、残り {} MCU ({}%) をグレーで埋めた",
                    tail.filled_mcus,
                    tail.percent()
                ));
                job.report.issue(format!(
                    "画像の下側 {}% は元のデータが失われている。グレーで埋めてあるだけで中身は戻らない",
                    tail.percent()
                ));
            }
            None => {
                out.extend_from_slice(&data[entropy_start..cut]);
                job.report.fixed("切れた末尾に EOI を付けた");
                job.report.issue(
                    "リスタートマーカー (DRI) が無いので、切れた位置から先を組み直せない。\
                     デコーダによっては末尾が乱れて見える",
                );
            }
        }
    } else {
        out.extend_from_slice(&data[entropy_start..cut]);
        let trailing = data.len() - (entropy_end + 2).min(data.len());
        if trailing > 0 {
            job.report.fixed(format!(
                "EOI の後ろに付いていた {trailing} バイトを落とした"
            ));
            status = status.or_repaired();
        }
    }

    // ---- EOI ----
    out.extend_from_slice(&[0xFF, 0xD9]);

    job.finish_image(&out, status)?;
    Ok(())
}

/// 組み立てたヘッダから、グレー埋めに要る情報を持ち回る。
struct HeaderInfo {
    sof: Option<Sof>,
    sos: Option<scan::Sos>,
    huffman: Vec<scan::HuffTable>,
    dri: Option<u16>,
}

/// 切れた末尾を埋める。戻り値は (埋めるデータ, エントロピー符号を切る位置)。
fn gray_fill(
    header: &HeaderInfo,
    found: &Jpeg,
    entropy_start: usize,
) -> Option<(fill::GrayTail, usize)> {
    let sof = header.sof.as_ref()?;
    let sos = header.sos.as_ref()?;
    let interval = header.dri.filter(|v| *v > 0)?;
    // 無事なリスタート間隔が 1 つも無い = 最初の間隔の途中で切れている。
    // ここから組み直すと画像全体がグレーになるだけなので手を出さない。
    let last_rst = found.last_rst?;
    if found.rst_count == 0 || last_rst < entropy_start {
        return None;
    }
    let tail = fill::gray_tail(sof, sos, &header.huffman, interval, found.rst_count)?;
    Some((tail, last_rst + 2))
}

/// 失われたヘッダを組み直す。
///
/// 元のファイルに残っているものを最優先し、無いものを参照ファイルから、
/// それも無ければ標準テーブルから埋める。全部を参照ファイルで置き換えないのは、
/// 元のファイルに残っている情報の方が必ず正確だから。
fn rebuild_header(
    job: &mut Job<'_>,
    data: &[u8],
    found: &Jpeg,
    reference: Option<&(Vec<u8>, Jpeg)>,
    out: &mut Vec<u8>,
) -> Result<HeaderInfo> {
    let (ref_data, ref_scan) = match reference {
        Some((d, s)) => (Some(d.as_slice()), Some(s)),
        None => (None, None),
    };
    let mut borrowed = Vec::new();

    out.extend_from_slice(&[0xFF, 0xD8]);

    // APPn (Exif など) は元のものだけを引き継ぐ。参照ファイルの Exif を
    // 混ぜると、別の写真の撮影日時が付いた偽の写真ができてしまう。
    for &(a, b) in &found.app {
        out.extend_from_slice(&data[a..b]);
    }

    // 量子化表。
    if !found.dqt.is_empty() {
        for &(a, b) in &found.dqt {
            out.extend_from_slice(&data[a..b]);
        }
    } else if let (Some(rd), Some(rs)) = (ref_data, ref_scan)
        && !rs.dqt.is_empty()
    {
        for &(a, b) in &rs.dqt {
            out.extend_from_slice(&rd[a..b]);
        }
        borrowed.push("量子化表");
    } else {
        out.extend_from_slice(&tables::standard_dqt());
        job.report.issue(
            "量子化表が失われていたので標準テーブルで代用した。\
             絵は出るが、色や階調は元どおりにならない",
        );
    }

    // フレームヘッダ(寸法とサンプリング)。
    let sof = match found.sof.clone() {
        Some(sof) => {
            out.extend_from_slice(&tables::sof0(sof.width, sof.height, &sof.components));
            sof
        }
        None => match ref_scan.and_then(|s| s.sof.clone()) {
            Some(mut sof) => {
                // 寸法だけは元のファイルの Exif の方が確かなことがある。
                if let Some((w, h)) = found.exif_size
                    && let (Ok(w), Ok(h)) = (u16::try_from(w), u16::try_from(h))
                {
                    sof.width = w;
                    sof.height = h;
                    borrowed.push("サンプリング設定 (寸法は元ファイルの Exif から)");
                } else {
                    borrowed.push("フレームヘッダ (寸法を含む)");
                }
                out.extend_from_slice(&tables::sof0(sof.width, sof.height, &sof.components));
                sof
            }
            None => {
                let Some((w, h)) = found
                    .exif_size
                    .or_else(|| job.options.size_hint())
                    .and_then(|(w, h)| Some((u16::try_from(w).ok()?, u16::try_from(h).ok()?)))
                else {
                    job.report.status = RepairStatus::Failed;
                    return Err(RepairError::NotEnoughInformation(
                        "画像の寸法が分からないので JPEG のヘッダを組み直せない。\
                         同じ機種で撮った正常なファイルを参照 (--reference) に指定するか、\
                         --width と --height で寸法を教えること"
                            .to_string(),
                    ));
                };
                let components = tables::default_components();
                out.extend_from_slice(&tables::sof0(w, h, &components));
                job.report.issue(
                    "サンプリング設定が分からないので一般的な 4:2:0 と仮定した。\
                     絵が崩れる場合は同じ機種で撮った正常なファイルを参照に指定すること",
                );
                Sof {
                    marker: 0xC0,
                    width: w,
                    height: h,
                    components,
                }
            }
        },
    };

    // ハフマン表。
    let huffman = if !found.dht.is_empty() {
        for &(a, b) in &found.dht {
            out.extend_from_slice(&data[a..b]);
        }
        found.huffman.clone()
    } else if let (Some(rd), Some(rs)) = (ref_data, ref_scan)
        && !rs.dht.is_empty()
    {
        for &(a, b) in &rs.dht {
            out.extend_from_slice(&rd[a..b]);
        }
        borrowed.push("ハフマン表");
        rs.huffman.clone()
    } else {
        let std = tables::standard_huffman();
        out.extend_from_slice(&tables::dht_segments(&std));
        std
    };

    // リスタート間隔。
    let dri = found.dri.or_else(|| ref_scan.and_then(|s| s.dri));
    if let Some(interval) = dri.filter(|v| *v > 0) {
        out.extend_from_slice(&tables::dri(interval));
    }

    // スキャンヘッダ。
    let sos = match found.sos.clone() {
        Some(sos) => {
            out.extend_from_slice(&tables::sos(&sos.components));
            sos
        }
        None => {
            let components = ref_scan
                .and_then(|s| s.sos.clone())
                .map(|s| s.components)
                .unwrap_or_else(|| tables::scan_components(&sof.components));
            out.extend_from_slice(&tables::sos(&components));
            scan::Sos {
                at: 0,
                header_end: 0,
                components,
            }
        }
    };

    let what = if borrowed.is_empty() {
        "標準テーブルからヘッダを組み直した".to_string()
    } else {
        format!(
            "ヘッダを組み直した (参照ファイルから借りたもの: {})",
            borrowed.join(", ")
        )
    };
    job.report.fixed(what);

    Ok(HeaderInfo {
        sof: Some(sof),
        sos: Some(sos),
        huffman,
        dri,
    })
}

/// 参照ファイルを読んで構造を取る。
fn load_reference(job: &mut Job<'_>) -> Result<Option<(Vec<u8>, Jpeg)>> {
    let Some(path) = job.reference else {
        return Ok(None);
    };
    let mut src = Source::open(path)?;
    let data = src.read_all(job.options.max_in_memory)?;
    let found = scan::scan(&data);
    if !found.header_is_usable() {
        return Err(RepairError::reference(
            path,
            "JPEG として読めない (参照ファイルは正常なものを指定すること)",
        ));
    }
    if found.sof.as_ref().is_some_and(|s| !s.is_sequential()) {
        return Err(RepairError::reference(
            path,
            "プログレッシブ JPEG は参照ファイルに使えない",
        ));
    }
    Ok(Some((data, found)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Verification;
    use crate::{RepairOptions, Repairer};
    use std::io::Write;

    /// 全面グレーのベースライン JPEG を組み立てる。
    ///
    /// エントロピー符号は [`fill::gray_tail`] にファイル全体を作らせている。
    /// これを `image` クレートがデコードできるということは、標準テーブルと
    /// 正準ハフマン符号の組み立てが仕様どおりだということでもある。
    fn synthetic(width: u16, height: u16, interval: u16) -> Vec<u8> {
        let components = tables::default_components();
        let sof = Sof {
            marker: 0xC0,
            width,
            height,
            components: components.clone(),
        };
        let sos = scan::Sos {
            at: 0,
            header_end: 0,
            components: tables::scan_components(&components),
        };
        let huffman = tables::standard_huffman();

        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(&tables::standard_dqt());
        out.extend_from_slice(&tables::sof0(width, height, &components));
        out.extend_from_slice(&tables::dht_segments(&huffman));
        out.extend_from_slice(&tables::dri(interval));
        out.extend_from_slice(&tables::sos(&sos.components));
        let all = fill::gray_tail(&sof, &sos, &huffman, interval, 0).expect("グレー埋めの生成");
        out.extend_from_slice(&all.data);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    fn write_temp(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        path
    }

    #[test]
    fn synthetic_jpeg_is_a_real_jpeg() {
        let data = synthetic(320, 240, 5);
        let img = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg)
            .expect("組み立てた JPEG をデコードできない");
        assert_eq!((img.width(), img.height()), (320, 240));

        let found = scan::scan(&data);
        assert!(found.header_is_usable());
        assert_eq!(found.dri, Some(5));
        assert!(found.eoi.is_some());
        // MCU 300 個を 5 個ずつ = 60 区間なので、区切りは 59 個。
        assert_eq!(found.rst_count, 59);
    }

    #[test]
    fn truncated_jpeg_is_filled_with_grey() {
        let dir = tempfile::tempdir().unwrap();
        let data = synthetic(320, 240, 5);
        let cut = data.len() * 3 / 4;
        let input = write_temp(&dir, "cut.jpg", &data[..cut]);
        let output = dir.path().join("fixed.jpg");

        let report = Repairer::new(&input, &output).run().unwrap();
        assert_eq!(report.status, RepairStatus::Partial, "{report:?}");
        assert!(
            matches!(
                report.verification,
                Verification::Decoded {
                    width: 320,
                    height: 240
                }
            ),
            "{:?}",
            report.verification
        );
        assert!(
            report.fixes.iter().any(|f| f.contains("グレーで埋めた")),
            "{:?}",
            report.fixes
        );
        // 埋めた分は元より短くなることはない (EOI とグレーの分だけ増える)。
        assert!(report.output_size >= cut as u64);
        // 出力は EOI で終わっている。寛容なデコーダは切れたままでも開くが、
        // 終端が無いファイルは扱いがソフト任せになる。そこを塞ぐのが修復の役目。
        let fixed = std::fs::read(&output).unwrap();
        assert_eq!(&fixed[fixed.len() - 2..], &[0xFF, 0xD9]);
    }

    #[test]
    fn trailing_junk_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = synthetic(64, 64, 2);
        let clean = data.len();
        data.extend_from_slice(&[0x5A; 500]);
        let input = write_temp(&dir, "junk.jpg", &data);
        let output = dir.path().join("fixed.jpg");

        let report = Repairer::new(&input, &output).run().unwrap();
        assert_eq!(report.status, RepairStatus::Repaired);
        assert_eq!(report.output_size, clean as u64);
        assert!(report.verification.passed());
    }

    #[test]
    fn intact_jpeg_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let data = synthetic(64, 64, 2);
        let input = write_temp(&dir, "ok.jpg", &data);
        let output = dir.path().join("copy.jpg");

        let report = Repairer::new(&input, &output).run().unwrap();
        assert_eq!(report.status, RepairStatus::Intact, "{report:?}");
        assert!(report.fixes.is_empty(), "{:?}", report.fixes);
        assert_eq!(std::fs::read(&output).unwrap(), data);
    }

    #[test]
    fn header_damage_without_a_reference_needs_the_size() {
        let dir = tempfile::tempdir().unwrap();
        let data = synthetic(64, 64, 2);
        // SOS の手前まで潰す。エントロピー符号だけが残った状態。
        let sos = scan::scan(&data).sos.unwrap().at;
        let mut broken = data.clone();
        for b in broken.iter_mut().take(sos) {
            *b = 0;
        }
        let input = write_temp(&dir, "head.jpg", &broken);

        // 寸法の手がかりが無ければ直せない。黙って適当な絵を作らないこと。
        let err = Repairer::new(&input, dir.path().join("a.jpg"))
            .run()
            .unwrap_err();
        assert!(
            matches!(err, crate::RepairError::NotEnoughInformation(_)),
            "{err}"
        );

        // 寸法を教えれば標準テーブルで組み直せる。
        let report = Repairer::new(&input, dir.path().join("b.jpg"))
            .with_options(RepairOptions {
                width: Some(64),
                height: Some(64),
                ..RepairOptions::default()
            })
            .run()
            .unwrap();
        assert!(report.status.produced_output(), "{report:?}");
        assert!(
            matches!(
                report.verification,
                Verification::Decoded {
                    width: 64,
                    height: 64
                }
            ),
            "{:?}",
            report.verification
        );
    }
}
