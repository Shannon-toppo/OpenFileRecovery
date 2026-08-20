//! カービングジョブ(PLAN.md 5.4)。
//!
//! 見つけたファイルは 1 件ずつイベントで流す(間引かない)。GUI は
//! 走査しながら結果ツリーを育てられる。

use std::sync::Arc;

use ofr_carve::{CarveOptions, Carver, FileFormat};
use ofr_device::Device;

use super::JobCtx;
use crate::dto::{CarveSummaryDto, CarvedFileDto, FormatCountDto, ProgressDto, eta_secs};
use crate::error::{CoreError, Result};
use crate::job::{CarveRequest, CarveResultDto, ItemDto, JobEvent, JobResult, NoteLevel, Outcome};
use crate::session::{CarveSession, Session};
use crate::source;

/// カービングを実行する。
pub(crate) fn run(ctx: &JobCtx, req: CarveRequest) -> Result<(Outcome, JobResult)> {
    source::check_source_selectable(&req.source)?;
    let device = source::open_source(&req.source)?;
    let info = device.info().clone();
    if info.is_system_disk {
        return Err(CoreError::SystemDisk(info.id));
    }
    source::check_destination(&info.id, &req.output)?;
    super::warn_if_live_device(ctx, &*device);

    if req.unmount {
        ctx.note(NoteLevel::Info, format!("{} をアンマウントする", info.id));
        ofr_device::unmount_device(&info.id)?;
    }

    let formats = if req.formats.is_empty() {
        None
    } else {
        let mut list = Vec::new();
        for name in &req.formats {
            let f = FileFormat::from_name(name)
                .ok_or_else(|| CoreError::BadRequest(format!("知らない形式: {name}")))?;
            if !list.contains(&f) {
                list.push(f);
            }
        }
        Some(list)
    };

    let options = CarveOptions {
        align: req.align,
        formats,
        max_file_size: req.max_size,
        start_offset: req.start.unwrap_or(0),
        end_offset: req.end,
        include_truncated: req.include_truncated,
        ..CarveOptions::default()
    };

    let reporter = ctx.clone();
    let finder = ctx.clone();
    let dest = req.output.clone();
    let report = Carver::new(&*device)
        .with_options(options)
        .with_cancel(Arc::clone(&ctx.cancel))
        .with_progress(move |p| {
            reporter.progress(ProgressDto {
                phase: "carving",
                position: p.position,
                total: p.end,
                ratio: p.ratio(),
                found: p.found,
                bytes_done: p.bytes_recovered,
                errors: p.read_errors,
                rate: p.rate,
                eta_secs: eta_secs(p.eta),
                elapsed_secs: p.elapsed.as_secs_f64(),
                ..ProgressDto::default()
            });
        })
        .with_found(move |f| {
            finder.emit(JobEvent::Item {
                job: finder.id,
                item: ItemDto::Carved(Box::new(CarvedFileDto::new(f, Some(&dest)))),
            });
        })
        .run(Some(&req.output))?;

    let report_path = req.output.join(crate::CARVE_REPORT_NAME);
    let by_format: Vec<FormatCountDto> = report
        .counts_by_format()
        .into_iter()
        .map(|(fmt, count)| FormatCountDto {
            format: fmt.name(),
            count,
            bytes: report
                .files
                .iter()
                .filter(|f| f.format == fmt)
                .map(|f| f.size)
                .sum(),
        })
        .collect();

    let s = &report.summary;
    let mut summary = CarveSummaryDto {
        scanned: s.scanned,
        found: s.found,
        exact: s.exact,
        bytes_recovered: s.bytes_recovered,
        read_errors: s.read_errors,
        elapsed_secs: s.elapsed.as_secs_f64(),
        cancelled: s.cancelled,
        by_format,
        output: Some(req.output.display().to_string()),
        report_path: None,
    };

    let files: Vec<CarvedFileDto> = report
        .files
        .iter()
        .map(|f| CarvedFileDto::new(f, Some(&req.output)))
        .collect();
    match write_report(&report_path, &req.source, &summary, &files) {
        Ok(()) => summary.report_path = Some(report_path.display().to_string()),
        Err(e) => ctx.note(
            NoteLevel::Warn,
            format!("レポートを作成できませんでした: {}", e.full_message()),
        ),
    }

    if s.found > 0 {
        ctx.note(
            NoteLevel::Info,
            "切り出したファイルは元の名前を持ちません。中身を確認してから整理してください。",
        );
    }

    // プレビューのためにデバイスと結果を残す。
    ctx.core.put_session(
        ctx.id,
        Session::Carve(Box::new(CarveSession {
            source: req.source.clone(),
            device,
            files: report.files,
            output: Some(req.output.clone()),
        })),
    );

    let complete = !s.cancelled && s.found > 0;
    Ok((
        ctx.outcome(complete),
        JobResult::Carve(Box::new(CarveResultDto {
            session: ctx.id,
            summary,
        })),
    ))
}

fn write_report(
    path: &std::path::Path,
    source: &str,
    summary: &CarveSummaryDto,
    files: &[CarvedFileDto],
) -> Result<()> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Report<'a> {
        source: &'a str,
        summary: &'a CarveSummaryDto,
        files: &'a [CarvedFileDto],
    }

    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| CoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    }
    let json = serde_json::to_vec_pretty(&Report {
        source,
        summary,
        files,
    })
    .map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    std::fs::write(path, json).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}
