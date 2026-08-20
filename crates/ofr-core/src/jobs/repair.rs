//! 修復ジョブ(PLAN.md 5.6)。
//!
//! 修復元は絶対に書き換えない。出力は必ず別のパスに書く(同じパスを渡すと
//! ofr-repair が開始前に拒否する)。参照ファイルを渡せる形式は精度が上がる。

use ofr_repair::{RepairFormat, RepairOptions, RepairStatus, Repairer};

use super::JobCtx;
use crate::dto::RepairReportDto;
use crate::error::{CoreError, Result};
use crate::job::{JobResult, NoteLevel, Outcome, RepairRequest};

/// 修復を実行する。
pub(crate) fn run(ctx: &JobCtx, req: RepairRequest) -> Result<(Outcome, JobResult)> {
    let format = match req.format.as_deref() {
        None | Some("auto") | Some("") => None,
        Some("jpeg") | Some("jpg") => Some(RepairFormat::Jpeg),
        Some("png") => Some(RepairFormat::Png),
        Some("avi") => Some(RepairFormat::Avi),
        Some("mp4") | Some("mov") => Some(RepairFormat::Mp4),
        Some(other) => {
            return Err(CoreError::BadRequest(format!("知らない形式: {other}")));
        }
    };

    let options = RepairOptions {
        format,
        verify: req.verify,
        width: req.width,
        height: req.height,
        ..RepairOptions::default()
    };

    let mut repairer = Repairer::new(&req.input, &req.output).with_options(options);
    if let Some(reference) = &req.reference {
        repairer = repairer.with_reference(reference);
    }
    let report = repairer.run()?;

    for issue in &report.issues {
        ctx.note(NoteLevel::Warn, issue.clone());
    }
    // MP4 の期待値と動画検証の但し書きは、GUI が report.format と
    // report.verification を見て翻訳済みの文言で出す。ここでは重ねない。

    let complete = matches!(report.status, RepairStatus::Intact | RepairStatus::Repaired);
    Ok((
        ctx.outcome(complete),
        JobResult::Repair(Box::new(RepairReportDto::from(&report))),
    ))
}
