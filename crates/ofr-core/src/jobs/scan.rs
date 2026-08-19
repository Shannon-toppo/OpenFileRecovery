//! 解析ジョブ(PLAN.md 5.3)。
//!
//! 結果のツリーはセッションとして core に残す。GUI はそれをページ単位で
//! 引き、選んだ項目の ID を復元ジョブへ渡す。

use std::sync::Arc;
use std::time::Duration;

use ofr_device::SliceDevice;
use ofr_fs::ScanOptions;

use super::JobCtx;
use crate::dto::{ProgressDto, ScanStatsDto, VolumeDto};
use crate::error::Result;
use crate::job::{JobResult, NoteLevel, Outcome, ScanRequest, ScanResultDto};
use crate::session::{ScanSession, Session};
use crate::source;

/// 解析を実行する。
pub(crate) fn run(ctx: &JobCtx, req: ScanRequest) -> Result<(Outcome, JobResult)> {
    source::check_source_selectable(&req.source)?;
    let device = source::open_source(&req.source)?;
    super::warn_if_live_device(ctx, &*device);

    let volume = source::locate(&*device, req.fs.kind(), req.offset)?;
    let region = SliceDevice::new(Arc::clone(&device), volume.offset, volume.len)?;
    let fs = source::open_filesystem(&region, volume.kind)?;
    let info = VolumeDto::new(fs.volume(), volume.offset, &volume.partition.type_name);

    if fs.volume().boot_source != ofr_fs::BootSource::Primary {
        ctx.note(
            NoteLevel::Warn,
            format!(
                "ブートセクタが読めなかったので {} を使った。ジオメトリが推定なら\
                 結果の信頼度は落ちる。",
                fs.volume().boot_source.label()
            ),
        );
    }

    let options = ScanOptions {
        deleted: req.deleted,
        orphans: req.orphans,
        cancel: Arc::clone(&ctx.cancel),
        progress_interval: Duration::from_millis(100),
        ..ScanOptions::default()
    };

    let reporter = ctx.clone();
    let tree = fs.scan(
        &options,
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

    let stats = ScanStatsDto::new(&tree);
    let warnings = tree.warnings.clone();
    let entry_count = tree.len();

    // 解析に使ったデバイスごと残す。復元とプレビューはここから読むので、
    // 壊れかけメディアを開き直さずに済む。
    let session = ScanSession {
        source: req.source.clone(),
        device,
        offset: volume.offset,
        len: volume.len,
        kind: volume.kind,
        tree,
    };
    ctx.core
        .put_session(ctx.id, Session::Scan(Box::new(session)));

    if stats.files == 0 && stats.dirs == 0 {
        ctx.note(
            NoteLevel::Warn,
            "何も見つからなかった。ファイルシステム自体が壊れている場合は\
             カービング(ファイル形式から探す)を試すこと。名前は戻らない。",
        );
    }

    let complete = stats.files + stats.dirs > 0 && !stats.cancelled;
    Ok((
        ctx.outcome(complete),
        JobResult::Scan(Box::new(ScanResultDto {
            session: ctx.id,
            volume: info,
            stats,
            entry_count,
            warnings,
        })),
    ))
}
