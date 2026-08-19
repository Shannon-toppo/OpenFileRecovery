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
    if report.format == RepairFormat::Mp4 && req.reference.is_none() {
        ctx.note(
            NoteLevel::Warn,
            "参照ファイルなしの MP4 修復は、市販ソフトを含めて成功率が大きく落ちる。\
             同じ機器・同じ設定で撮った正常なファイルを渡すと精度が上がる。",
        );
    }
    if report.verification == ofr_repair::Verification::Container {
        ctx.note(
            NoteLevel::Info,
            "動画の自動検証はコンテナ整合性まで。実際に再生できるかは\
             プレイヤーで確かめること。",
        );
    }

    let complete = matches!(report.status, RepairStatus::Intact | RepairStatus::Repaired);
    Ok((
        ctx.outcome(complete),
        JobResult::Repair(Box::new(RepairReportDto::from(&report))),
    ))
}
