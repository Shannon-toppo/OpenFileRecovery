//! Open File Recovery の公開 API。
//!
//! 下の解析クレート(デバイス / イメージング / FAT32 / exFAT / カービング /
//! コピー / 修復)を束ねて、GUI と CLI が同じ入口から使えるようにしたもの
//! (PLAN.md 3章 `ofr-core`)。
//!
//! # 使い方
//!
//! 時間のかかる処理は全部「ジョブ」にしてある。ジョブは開始するとスレッドで
//! 走り、進捗・見つけた項目・完了をイベントで流す。キャンセルは
//! [`Core::cancel`]、途中まで書き出したものはそのまま残る。
//!
//! ```no_run
//! use std::sync::Arc;
//! use ofr_core::{Core, JobRequest, ScanRequest};
//!
//! let core = Core::new();
//! let job = core.start(
//!     JobRequest::Scan(serde_json::from_str::<ScanRequest>(
//!         r#"{"source": "usb.img"}"#,
//!     )?),
//!     Arc::new(|event| println!("{}", serde_json::to_string(&event).unwrap())),
//! )?;
//!
//! // 解析が終わったら、その ID がそのままセッション ID になる。
//! // core.entries(job, &query) で結果を引き、core.preview(job, id, 0) で中身を見る。
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # 安全原則
//!
//! 復旧元への書き込み経路は下位にも存在しない(PLAN.md 6章 1項)。
//! 起動ディスクの拒否(同 3項)と「出力先が復旧元と同じデバイス」の拒否
//! (同 2項)は [`source`] にまとめてあり、CLI と GUI の両方がここを通る。

#![deny(unsafe_code)]

pub mod dto;
pub mod elevate;
mod error;
pub mod filter;
mod job;
mod jobs;
pub mod session;
pub mod source;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use dto::{
    CarveSummaryDto, CarvedFileDto, ConcernsDto, CopySummaryDto, DeviceDto, EntryDto,
    FileResultDto, ImageSummaryDto, MapSegmentDto, ProgressDto, RepairReportDto, ScanStatsDto,
    VolumeDto,
};
pub use elevate::PrivilegeDto;
pub use error::{CoreError, ErrorCode, Result};
pub use job::{
    CarveRequest, CarveResultDto, CopyRequest, CopyResultDto, EventSink, ExistingFile, FsChoice,
    ImageRequest, ItemDto, JobEvent, JobId, JobKind, JobRequest, JobResult, NoteLevel, Outcome,
    RepairRequest, RestoreRequest, ScanRequest, ScanResultDto,
};
pub use session::{
    CarveSession, DEFAULT_PREVIEW_LIMIT, EntryPage, EntryQuery, PreviewDto, ScanSession, Session,
};

use jobs::JobCtx;

/// 復元したときに宛先へ書くレポートの名前。
pub const RESTORE_REPORT_NAME: &str = "ofr-restore-report.json";
/// カービングしたときに出力先へ書くレポートの名前。
pub const CARVE_REPORT_NAME: &str = "carve-report.json";

/// 走っているジョブ 1 本の記録。
struct JobEntry {
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

/// ジョブとセッションの置き場。GUI はこれを 1 つ持つ。
#[derive(Default)]
pub struct Core {
    next_id: AtomicU64,
    jobs: Mutex<HashMap<JobId, JobEntry>>,
    sessions: Mutex<HashMap<JobId, Arc<Session>>>,
}

impl Core {
    /// 空の状態で作る。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 接続されているデバイスを列挙する(PLAN.md 5.1)。
    ///
    /// 起動ディスクも一覧には出るが `selectable` が偽になる。列挙自体は
    /// 管理者権限なしでも動く。
    pub fn devices(&self) -> Result<Vec<DeviceDto>> {
        Ok(ofr_device::list_devices()?
            .iter()
            .map(DeviceDto::from)
            .collect())
    }

    /// いまの権限。
    pub fn privileges(&self) -> PrivilegeDto {
        elevate::state()
    }

    /// ジョブを始める。戻り値はジョブ ID。
    ///
    /// 実処理は専用スレッドで走る。結果はすべて `sink` に流れるので、
    /// 呼び出し側はここで待たない。
    pub fn start(self: &Arc<Self>, request: JobRequest, sink: EventSink) -> Result<JobId> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let kind = request.kind();
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));

        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            JobEntry {
                kind,
                cancel: Arc::clone(&cancel),
                running: Arc::clone(&running),
            },
        );

        let ctx = JobCtx {
            id,
            sink,
            cancel,
            core: Arc::clone(self),
        };
        let (source, dest) = describe(&request);

        let spawned = std::thread::Builder::new()
            .name(format!("ofr-job-{id}"))
            .spawn(move || {
                ctx.emit(JobEvent::Started {
                    job: id,
                    kind,
                    source: source.clone(),
                    dest,
                });
                match jobs::run(&ctx, request) {
                    Ok((outcome, result)) => ctx.emit(JobEvent::Finished {
                        job: id,
                        outcome,
                        result: Box::new(result),
                    }),
                    Err(e) => {
                        let mut message = e.full_message();
                        if let Some(hint) = e.hint(&source) {
                            message.push('\n');
                            message.push_str(&hint);
                        }
                        tracing::warn!(job = id, "{message}");
                        ctx.emit(JobEvent::Failed {
                            job: id,
                            code: e.code(),
                            message,
                        });
                    }
                }
                running.store(false, Ordering::Relaxed);
            });

        if let Err(e) = spawned {
            self.jobs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(CoreError::Io {
                path: std::path::PathBuf::from("(thread)"),
                source: e,
            });
        }
        Ok(id)
    }

    /// ジョブを中断する。書き出し済みのものは残る。
    ///
    /// 知らない ID なら偽。
    pub fn cancel(&self, job: JobId) -> bool {
        match self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job)
        {
            Some(entry) => {
                entry.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// 走っているジョブを全部中断する。アプリを閉じるときに呼ぶ。
    pub fn cancel_all(&self) {
        for entry in self.jobs.lock().unwrap_or_else(|e| e.into_inner()).values() {
            entry.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// まだ走っているか。
    pub fn is_running(&self, job: JobId) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job)
            .is_some_and(|e| e.running.load(Ordering::Relaxed))
    }

    /// ジョブの種類。
    pub fn kind_of(&self, job: JobId) -> Option<JobKind> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&job)
            .map(|e| e.kind)
    }

    /// 解析結果を 1 ページ取り出す。
    ///
    /// 数十万件になりうるので、GUI は絞り込みと組み合わせて少しずつ引く。
    pub fn entries(&self, session: JobId, query: &EntryQuery) -> Result<EntryPage> {
        match &*self.require_session(session)? {
            Session::Scan(s) => Ok(s.page(query)),
            Session::Carve(_) => Err(CoreError::BadRequest(
                "このセッションは解析結果ではない".to_string(),
            )),
        }
    }

    /// カービング結果の一覧。
    pub fn carved(&self, session: JobId) -> Result<Vec<CarvedFileDto>> {
        match &*self.require_session(session)? {
            Session::Carve(c) => Ok(session::carved_dtos(c)),
            Session::Scan(_) => Err(CoreError::BadRequest(
                "このセッションはカービング結果ではない".to_string(),
            )),
        }
    }

    /// 中身をプレビュー用に読み出す。`limit` が 0 なら
    /// [`DEFAULT_PREVIEW_LIMIT`] まで。
    pub fn preview(&self, session: JobId, index: usize, limit: u64) -> Result<PreviewDto> {
        self.require_session(session)?.preview(index, limit)
    }

    /// セッションを捨てる。開いていたデバイスもここで閉じる。
    pub fn close_session(&self, session: JobId) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session);
    }

    /// セッションを引く。
    pub fn session(&self, id: JobId) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .map(Arc::clone)
    }

    fn require_session(&self, id: JobId) -> Result<Arc<Session>> {
        self.session(id).ok_or(CoreError::NoSession(id))
    }

    pub(crate) fn put_session(&self, id: JobId, session: Session) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::new(session));
    }
}

/// 開始イベントに載せる復旧元と出力先。
fn describe(request: &JobRequest) -> (String, Option<String>) {
    match request {
        JobRequest::Image(r) => (r.source.clone(), Some(r.output.display().to_string())),
        JobRequest::Scan(r) => (r.source.clone(), None),
        JobRequest::Restore(r) => (
            format!("session {}", r.session),
            Some(r.dest.display().to_string()),
        ),
        JobRequest::Carve(r) => (r.source.clone(), Some(r.output.display().to_string())),
        JobRequest::Copy(r) => (r.source.clone(), Some(r.dest.display().to_string())),
        JobRequest::Repair(r) => (
            r.input.display().to_string(),
            Some(r.output.display().to_string()),
        ),
    }
}
