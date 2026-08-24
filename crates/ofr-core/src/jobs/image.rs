//! 吸い出しジョブ(PLAN.md 5.2)。

use std::path::PathBuf;
use std::time::Duration;

use ofr_device::Device;
use ofr_image::{ImageOptions, Imager, StallAction};

use super::JobCtx;
use crate::dto::{ImageSummaryDto, MapSegmentDto, ProgressDto, eta_secs};
use crate::error::{CoreError, Result};
use crate::job::{ImageRequest, JobResult, NoteLevel, Outcome};
use crate::replug::{self, ReplugState};
use crate::source;

/// 吸い出しを実行する。
pub(crate) fn run(ctx: &JobCtx, req: ImageRequest) -> Result<(Outcome, JobResult)> {
    // 6章 3項: 起動ディスクは復旧元にできない。開く前に弾く。
    source::check_source_selectable(&req.source)?;

    // 固まったメディアで読み込みが返らなくなっても止まらないよう、制限時間を設ける。
    let io_timeout = Duration::from_secs_f64(req.io_timeout_secs.max(0.0));
    let device = source::open_source_with_timeout(
        &req.source,
        io_timeout,
        ofr_device::DEFAULT_MAX_ABANDONED,
    )?;
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

    // 読む範囲の限定(PLAN.md 11.4)。走査結果から作った mapfile を渡してもらう。
    let domain = match &req.domain {
        Some(path) => {
            let domain = ofr_image::Domain::load(path)?;
            if domain.is_empty() {
                return Err(CoreError::BadRequest(format!(
                    "{} に取得対象の領域がありません",
                    path.display()
                )));
            }
            ctx.note(
                NoteLevel::Info,
                format!(
                    "{} が示す {} 分だけを読みます",
                    path.display(),
                    domain.total()
                ),
            );
            Some(domain)
        }
        None => None,
    };

    // ブートセクタ・FAT 表・ルートディレクトリを先に確保する(PLAN.md 11.4)。
    // 途中で読めなくなっても構造だけは残るようにするため。位置が分からなければ
    // 何もせず普通に頭から読む。
    // 再試行だけのときはコピーパスに入らないので、割り出す意味がない(そのぶん
    // 弱ったメディアを余計に読まない)。
    let priority =
        (req.metadata_first && !req.retry_only).then(|| metadata_priority(ctx, &*device));

    let options = ImageOptions {
        chunk_size: req.block_size,
        retries: req.retries,
        trim: req.trim,
        scrape: req.scrape,
        retry: req.retry,
        retry_only: req.retry_only,
        reset_bad: req.reset_bad,
        domain,
        priority: priority.flatten(),
        reverse: req.reverse,
        alt_route: req.alt_route,
        verify: req.verify,
        progress_interval: Duration::from_millis(100),
        ..ImageOptions::default()
    };

    let reporter = ctx.clone();
    let mut imager = Imager::new(&*device)
        .with_options(options)
        .with_cancel(ctx.cancel.clone())
        .with_progress(move |p| {
            reporter.progress(ProgressDto {
                phase: match p.pass {
                    ofr_image::Pass::Priority => "metadata",
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
        });

    // 固まったコントローラは電源を入れ直す以外に戻らない。抜き差しを待つ
    // (PLAN.md 11.3)。GUI は待っている間の状態を注記で受け取る。
    if req.wait_on_stall {
        let waiter = ctx.clone();
        let identity = info.clone();
        let wait_timeout = Duration::from_secs_f64(req.wait_timeout_secs.max(0.0));
        imager = imager.with_stall_handler(move |stall| {
            waiter.note(
                NoteLevel::Warn,
                format!(
                    "デバイスが応答しません({}回目)。メディアをいったん抜いて、数秒おいてから挿し直してください。",
                    stall.attempt
                ),
            );
            let back = replug::wait_for_replug(
                &identity,
                wait_timeout,
                &waiter.cancel,
                |state| match state {
                    ReplugState::WaitingInsertion => {
                        waiter.note(NoteLevel::Info, "デバイスが外れました。挿し直してください。");
                    }
                    ReplugState::Back(id) => {
                        waiter.note(NoteLevel::Info, format!("{id} が戻りました。続きから読み直します。"));
                    }
                    ReplugState::TimedOut => {
                        waiter.note(NoteLevel::Warn, "待ちましたがデバイスが戻ってこないので打ち切ります。");
                    }
                    _ => {}
                },
            );
            if back.is_some() {
                StallAction::Resume
            } else {
                StallAction::Abort
            }
        });
    }

    let summary = imager.run(&req.output, Some(&map_path))?;

    if !summary.is_complete() && !summary.cancelled {
        ctx.note(
            NoteLevel::Warn,
            "読めない領域が残っています。時間をおいて同じ設定で実行すると、mapfileの不良領域だけを再試行します。",
        );
    }
    if summary.uncertain > 0 {
        ctx.note(
            NoteLevel::Warn,
            format!(
                "{}バイトぶんは、読むたびに中身が変わりました。取り出したファイルが壊れている可能性があります。",
                summary.uncertain
            ),
        );
    }
    if summary.timeouts > 0 {
        ctx.note(
            NoteLevel::Warn,
            format!(
                "{}回、読み込みが返ってこなかったので見捨てました。メディアをいったん抜いて挿し直してから再開すると、読めることがあります。",
                summary.timeouts
            ),
        );
    }

    let dto = ImageSummaryDto {
        total: summary.total,
        rescued: summary.rescued,
        bad: summary.bad,
        remaining: summary.remaining,
        errors: summary.errors,
        timeouts: summary.timeouts,
        reopens: summary.reopens,
        stalls: summary.stalls,
        alt_rescues: summary.alt_rescues,
        resized_rescues: summary.resized_rescues,
        uncertain: summary.uncertain,
        elapsed_secs: summary.elapsed.as_secs_f64(),
        cancelled: summary.cancelled,
        complete: summary.is_complete(),
        image_path: req.output.display().to_string(),
        map_path: Some(map_path.display().to_string()),
    };
    Ok((ctx.outcome(dto.complete), JobResult::Image(dto)))
}

/// メタデータの位置を割り出して、先に確保する範囲にする(PLAN.md 11.4)。
///
/// 割り出せなかった場合は `None`。ここで失敗しても吸い出し自体は続けられるので、
/// 注記だけ出して先へ進む。
fn metadata_priority(ctx: &JobCtx, device: &dyn Device) -> Option<ofr_image::Domain> {
    let plan = crate::domain::metadata_plan(device);
    if plan.is_empty() {
        ctx.note(
            NoteLevel::Info,
            "メタデータの位置が分からないので、先頭から順に読みます。",
        );
        return None;
    }
    ctx.note(
        NoteLevel::Info,
        format!(
            "ブートセクタやFAT表など{}か所({}バイト)を先に確保します。",
            plan.regions.len(),
            plan.domain.total()
        ),
    );
    Some(plan.domain)
}
