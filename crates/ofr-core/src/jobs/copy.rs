//! コピージョブ(PLAN.md 5.5)。
//!
//! 復旧元の指定で読み出し経路が決まる。フォルダならマウント済みデバイスの
//! 論理コピー、デバイス ID / イメージなら FAT32 / exFAT を直読みして展開する。
//! どちらでも宛先にできるミラーツリーとレポートは同じ形になる。

use std::path::Path;
use std::sync::Arc;

use ofr_copy::{
    Copier, CopyOptions, CopySource, MountSource, REPORT_JSON, REPORT_TEXT, TreeSource,
};
use ofr_device::{Device, SliceDevice};
use ofr_fs::{EntryStatus, ScanOptions};

use super::JobCtx;
use crate::dto::{CopySummaryDto, FileResultDto, ProgressDto, eta_secs};
use crate::error::{CoreError, Result};
use crate::job::{CopyRequest, CopyResultDto, ItemDto, JobEvent, JobResult, NoteLevel, Outcome};
use crate::source;

/// コピーを実行する。
pub(crate) fn run(ctx: &JobCtx, req: CopyRequest) -> Result<(Outcome, JobResult)> {
    if Path::new(&req.source).is_dir() {
        run_logical(ctx, req)
    } else {
        run_raw(ctx, req)
    }
}

/// マウント済みフォルダからの論理コピー。
fn run_logical(ctx: &JobCtx, req: CopyRequest) -> Result<(Outcome, JobResult)> {
    let root = Path::new(&req.source);
    // 6章 2項: 宛先が復旧元と同じデバイス上にあってはいけない。
    // (宛先が復旧元フォルダの中にある場合は ofr-copy 側が弾く。)
    if let Some(disk) = ofr_device::disk_id_for_path(root) {
        source::check_destination(&disk, &req.dest)?;
    }
    let mount = MountSource::new(root);
    copy(ctx, &mount, &req)
}

/// デバイス / イメージからの直読みコピー(イメージ展開)。
fn run_raw(ctx: &JobCtx, req: CopyRequest) -> Result<(Outcome, JobResult)> {
    source::check_source_selectable(&req.source)?;
    let device = source::open_source(&req.source)?;
    let info = device.info().clone();
    source::check_destination(&info.id, &req.dest)?;
    super::warn_if_live_device(ctx, &*device);

    let volume = source::locate(&*device, req.fs.kind(), req.offset)?;
    let region = SliceDevice::new(Arc::clone(&device), volume.offset, volume.len)?;
    let fs = source::open_filesystem(&region, volume.kind)?;

    ctx.note(
        NoteLevel::Info,
        format!("{} を直読みして展開する", fs.volume().kind),
    );

    // コピーは「いま入っているもの」が対象。削除済みを足すときだけ
    // 孤立クラスタ走査まで回す(時間がかかるため)。
    let scan_options = ScanOptions {
        deleted: req.include_deleted,
        orphans: req.include_deleted,
        cancel: Arc::clone(&ctx.cancel),
        ..ScanOptions::default()
    };
    let reporter = ctx.clone();
    let tree = fs.scan(
        &scan_options,
        Some(Box::new(move |p: &ofr_fs::ScanProgress| {
            reporter.progress(ProgressDto {
                phase: match p.phase {
                    ofr_fs::ScanPhase::Directories => "directories",
                    ofr_fs::ScanPhase::Orphans => "orphans",
                },
                position: p.position,
                total: p.total,
                ratio: if p.total > 0 {
                    p.position as f64 / p.total as f64
                } else {
                    0.0
                },
                found: p.found as u64,
                elapsed_secs: p.elapsed.as_secs_f64(),
                ..ProgressDto::default()
            });
        })),
    )?;

    let mut tree_source = TreeSource::new(&region, &tree).with_label(&req.source);
    if req.include_deleted {
        tree_source = tree_source.with_statuses([
            EntryStatus::Intact,
            EntryStatus::Deleted,
            EntryStatus::Orphaned,
            EntryStatus::Damaged,
        ]);
    }
    copy(ctx, &tree_source, &req)
}

fn copy(ctx: &JobCtx, src: &dyn CopySource, req: &CopyRequest) -> Result<(Outcome, JobResult)> {
    let options = CopyOptions {
        retries: req.retries,
        chunk_size: req.chunk_size as usize,
        zero_fill: req.zero_fill,
        set_timestamps: req.timestamps,
        on_existing: req.on_existing.into(),
        ..CopyOptions::default()
    };

    let reporter = ctx.clone();
    let done = ctx.clone();
    let report = Copier::new(src, &req.dest)
        .with_options(options)
        .with_cancel(Arc::clone(&ctx.cancel))
        .with_progress(move |p| {
            reporter.progress(ProgressDto {
                phase: "copying",
                items_done: p.files_done,
                items_total: p.files_total,
                bytes_done: p.bytes_done,
                bytes_total: p.bytes_total,
                ratio: p.ratio(),
                errors: p.failed,
                rate: p.rate,
                eta_secs: eta_secs(p.eta),
                elapsed_secs: p.elapsed.as_secs_f64(),
                current: p.current.clone(),
                ..ProgressDto::default()
            });
        })
        .with_file_done(move |f| {
            done.emit(JobEvent::Item {
                job: done.id,
                item: ItemDto::File(Box::new(FileResultDto::from(f))),
            });
        })
        .run()?;

    let mut summary = CopySummaryDto::new(&report.summary, &req.dest);
    let json = req.dest.join(REPORT_JSON);
    let text = req.dest.join(REPORT_TEXT);
    match write_reports(&report, &json, &text) {
        Ok(()) => {
            summary.report_json = Some(json.display().to_string());
            summary.report_text = Some(text.display().to_string());
        }
        Err(e) => ctx.note(
            NoteLevel::Warn,
            format!("レポートを作成できませんでした: {}", e.full_message()),
        ),
    }

    if report.summary.partial > 0 || report.summary.failed > 0 {
        ctx.note(
            NoteLevel::Warn,
            "読めなかった部分はゼロで埋めています。開けないファイルは修復を試してください。",
        );
    }
    if report.summary.cancelled {
        ctx.note(
            NoteLevel::Info,
            "中断しました。同名の扱いを「飛ばす」にして実行し直すと続きから進みます。",
        );
    }

    let incomplete: Vec<FileResultDto> = report
        .incomplete_files()
        .take(200)
        .map(FileResultDto::from)
        .collect();

    Ok((
        ctx.outcome(summary.complete),
        JobResult::Copy(CopyResultDto {
            summary,
            incomplete,
        }),
    ))
}

fn write_reports(report: &ofr_copy::CopyReport, json: &Path, text: &Path) -> Result<()> {
    if let Some(dir) = json.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| CoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    }
    report.write_json(json)?;
    report.write_text(text)?;
    Ok(())
}
