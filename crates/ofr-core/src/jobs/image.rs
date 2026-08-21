//! 吸い出しジョブ(PLAN.md 5.2)。

use std::path::PathBuf;
use std::time::Duration;

use ofr_device::Device;
use ofr_image::{ImageOptions, Imager};

use super::JobCtx;
use crate::dto::{ImageSummaryDto, MapSegmentDto, ProgressDto, eta_secs};
use crate::error::{CoreError, Result};
use crate::job::{ImageRequest, JobResult, NoteLevel, Outcome};
use crate::source;

/// 吸い出しを実行する。
pub(crate) fn run(ctx: &JobCtx, req: ImageRequest) -> Result<(Outcome, JobResult)> {
    // 6章 3項: 起動ディスクは復旧元にできない。開く前に弾く。
    source::check_source_selectable(&req.source)?;

    let device = source::open_source(&req.source)?;
    let info = device.info().clone();
    if info.is_system_disk {
        return Err(CoreError::SystemDisk(info.id.clone()));
    }
    // 6章 2項: 出力先が復旧元と同じデバイス上にあってはいけない。
    source::check_destination(&info.id, &req.output)?;

    if req.unmount {
        ctx.note(NoteLevel::Info, format!("{} をアンマウントする", info.id));
        ofr_device::unmount_device(&info.id)?;
    }

    let map_path: PathBuf = req
        .mapfile
        .clone()
        .unwrap_or_else(|| crate::mapfile_path(&req.output));

    // 再開できない上書きは、GUI 側で確認を取ってから overwrite を立てて来る。
    if req.output.exists() && !req.overwrite && !map_path.exists() {
        return Err(CoreError::BadRequest(format!(
            "{} は既に存在します。再開用のmapfileがないので、上書きの確認が必要です",
            req.output.display()
        )));
    }
    if map_path.exists() {
        ctx.note(
            NoteLevel::Info,
            format!("{} から再開する", map_path.display()),
        );
    }

    let options = ImageOptions {
        chunk_size: req.block_size,
        retries: req.retries,
        trim: req.trim,
        scrape: req.scrape,
        retry: req.retry,
        progress_interval: Duration::from_millis(100),
        ..ImageOptions::default()
    };

    let reporter = ctx.clone();
    let summary = Imager::new(&*device)
        .with_options(options)
        .with_cancel(ctx.cancel.clone())
        .with_progress(move |p| {
            reporter.progress(ProgressDto {
                phase: match p.pass {
                    ofr_image::Pass::Copy => "copy",
                    ofr_image::Pass::Trim => "trim",
                    ofr_image::Pass::Scrape => "scrape",
                    ofr_image::Pass::Retry => "retry",
                },
                pass: p.pass_number,
                position: p.position,
                total: p.total,
                ratio: if p.total > 0 {
                    p.rescued as f64 / p.total as f64
                } else {
                    0.0
                },
                rescued: p.rescued,
                bad: p.bad,
                pending: p.pending,
                errors: p.errors,
                rate: p.rate,
                eta_secs: eta_secs(p.eta),
                elapsed_secs: p.elapsed.as_secs_f64(),
                map: p.map.iter().map(MapSegmentDto::from).collect(),
                ..ProgressDto::default()
            });
        })
        .run(&req.output, Some(&map_path))?;

    if !summary.is_complete() && !summary.cancelled {
        ctx.note(
            NoteLevel::Warn,
            "読めない領域が残っています。時間をおいて同じ設定で実行すると、mapfileの不良領域だけを再試行します。",
        );
    }

    let dto = ImageSummaryDto {
        total: summary.total,
        rescued: summary.rescued,
        bad: summary.bad,
        remaining: summary.remaining,
        errors: summary.errors,
        reopens: summary.reopens,
        elapsed_secs: summary.elapsed.as_secs_f64(),
        cancelled: summary.cancelled,
        complete: summary.is_complete(),
        image_path: req.output.display().to_string(),
        map_path: Some(map_path.display().to_string()),
    };
    Ok((ctx.outcome(dto.complete), JobResult::Image(dto)))
}
