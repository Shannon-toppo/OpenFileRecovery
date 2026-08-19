//! ジョブの実装。
//!
//! それぞれ「スレッドで走り、進捗をイベントで流し、キャンセルフラグを見る」
//! という同じ形にしてある(PLAN.md 4章)。CLI が `ofr scan` などでやることを、
//! 標準出力ではなくイベントに置き換えたものと考えてよい。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dto::ProgressDto;
use crate::error::Result;
use crate::job::{EventSink, JobEvent, JobId, JobResult, NoteLevel, Outcome};

pub(crate) mod carve;
pub(crate) mod copy;
pub(crate) mod image;
pub(crate) mod repair;
pub(crate) mod restore;
pub(crate) mod scan;

/// ジョブ 1 本ぶんの実行環境。進捗の送り先とキャンセルフラグを持つ。
#[derive(Clone)]
pub(crate) struct JobCtx {
    /// ジョブ ID。
    pub id: JobId,
    /// イベントの送り先。
    pub sink: EventSink,
    /// キャンセルフラグ。
    pub cancel: Arc<AtomicBool>,
    /// セッションの置き場。
    pub core: Arc<crate::Core>,
}

impl JobCtx {
    /// イベントを 1 つ流す。
    pub fn emit(&self, event: JobEvent) {
        (self.sink)(event);
    }

    /// 進捗を流す。
    pub fn progress(&self, progress: ProgressDto) {
        self.emit(JobEvent::Progress {
            job: self.id,
            progress: Box::new(progress),
        });
    }

    /// 注記を流す。
    pub fn note(&self, level: NoteLevel, message: impl Into<String>) {
        self.emit(JobEvent::Note {
            job: self.id,
            level,
            message: message.into(),
        });
    }

    /// 中断されたか。
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 中断の有無から結末を決める。
    pub fn outcome(&self, complete: bool) -> Outcome {
        if self.cancelled() {
            Outcome::Cancelled
        } else if complete {
            Outcome::Complete
        } else {
            Outcome::Incomplete
        }
    }
}

/// 依頼を実行する。
pub(crate) fn run(ctx: &JobCtx, request: crate::job::JobRequest) -> Result<(Outcome, JobResult)> {
    use crate::job::JobRequest as R;
    match request {
        R::Image(r) => image::run(ctx, r),
        R::Scan(r) => scan::run(ctx, r),
        R::Restore(r) => restore::run(ctx, r),
        R::Carve(r) => carve::run(ctx, r),
        R::Copy(r) => copy::run(ctx, r),
        R::Repair(r) => repair::run(ctx, r),
    }
}

/// 壊れかけメディアを直接触ることへの注意(PLAN.md 6章 4項)。
///
/// GUI は「まずイメージを取ってから解析」を標準フローとして誘導するが、
/// 直接解析の近道も残してあるので、通るたびに一言添える。
pub(crate) fn warn_if_live_device(ctx: &JobCtx, device: &dyn ofr_device::Device) {
    if device.info().kind != ofr_device::DeviceKind::ImageFile {
        ctx.note(
            NoteLevel::Warn,
            "デバイスを直接読んでいる。読み出しが不安定なメディアなら、\
             先にイメージを取り、そのイメージを開くこと。",
        );
    }
}
