//! コピーのジョブ本体。
//!
//! やることは単純で、[`CopySource`] が集めた項目を順に宛先へ書くだけ。
//! ただし相手が壊れかけメディアなので、次の 2 点を守る(PLAN.md 5.5):
//!
//! - 1 ファイルが読めなくても止まらない。読めた分を書き、記録して次へ進む
//! - 何がどこまでコピーできたかを必ずレポートに残す
//!
//! IO はシーケンシャル 1 本(PLAN.md 5.7)。壊れかけデバイスへの並列アクセスは
//! コントローラを詰まらせるので、ここでスレッドを増やさない。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ofr_fs::extract;

use crate::error::Result;
use crate::options::{CopyOptions, ExistingFile};
use crate::progress::{CopyProgress, FileDoneFn, ProgressFn};
use crate::report::{CopyReport, CopyStatus, CopySummary, FileResult};
use crate::source::{CopyItem, CopySource};

/// コピージョブ。
///
/// ```no_run
/// use ofr_copy::{Copier, MountSource};
///
/// let source = MountSource::new("/Volumes/USB");
/// let report = Copier::new(&source, "/Volumes/Backup/mirror")
///     .with_file_done(|f| println!("{} {}", f.status, f.source))
///     .run()?;
///
/// println!("{}", report.text_summary());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Copier<'a> {
    source: &'a dyn CopySource,
    dest: PathBuf,
    options: CopyOptions,
    cancel: Arc<AtomicBool>,
    progress: Option<ProgressFn>,
    file_done: Option<FileDoneFn>,
}

impl<'a> Copier<'a> {
    /// 既定の設定でジョブを作る。
    pub fn new(source: &'a dyn CopySource, dest: impl Into<PathBuf>) -> Self {
        Self {
            source,
            dest: dest.into(),
            options: CopyOptions::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: None,
            file_done: None,
        }
    }

    /// 設定を差し替える。
    pub fn with_options(mut self, options: CopyOptions) -> Self {
        self.options = options;
        self
    }

    /// キャンセルフラグを共有する。真になった時点で、ファイルの区切りで中断する。
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// 進捗コールバックを設定する(既定 100ms 間隔に間引かれる)。
    pub fn with_progress(mut self, f: impl FnMut(&CopyProgress) + Send + 'static) -> Self {
        self.progress = Some(Box::new(f));
        self
    }

    /// 1 ファイル終わるごとのコールバックを設定する(間引かない)。
    pub fn with_file_done(mut self, f: impl FnMut(&FileResult) + Send + 'static) -> Self {
        self.file_done = Some(Box::new(f));
        self
    }

    /// 何をコピーするかだけ調べる(`--dry-run` 用)。何も書かない。
    pub fn plan(&self) -> Result<Vec<CopyItem>> {
        self.source.check_destination(&self.dest)?;
        self.source.collect(&self.cancel)
    }

    /// コピーを実行する。
    pub fn run(mut self) -> Result<CopyReport> {
        let started = Instant::now();
        self.source.check_destination(&self.dest)?;
        let items = self.source.collect(&self.cancel)?;
        extract::create_dir(&self.dest)?;

        let files: Vec<&CopyItem> = items.iter().filter(|i| !i.is_dir()).collect();
        let mut summary = CopySummary {
            files: files.len() as u64,
            bytes_expected: files.iter().map(|i| i.size).sum(),
            ..CopySummary::default()
        };
        let mut results = Vec::with_capacity(files.len());
        let mut last_progress: Option<Instant> = None;

        for item in &items {
            if self.cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }

            if item.is_dir() {
                if self.options.create_empty_dirs {
                    let path = self.output_path(item);
                    match extract::create_dir(&path) {
                        Ok(()) => summary.dirs += 1,
                        Err(e) => {
                            // フォルダを 1 つ作れなくても、中のファイルは
                            // 親を作り直しながら書ける。止めずに記録だけする。
                            tracing::warn!("{} を作れない: {e}", path.display());
                        }
                    }
                }
                continue;
            }

            let result = self.copy_one(item);
            match result.status {
                CopyStatus::Copied => summary.copied += 1,
                CopyStatus::Partial => summary.partial += 1,
                CopyStatus::Failed => summary.failed += 1,
                CopyStatus::Skipped => summary.skipped += 1,
            }
            summary.bytes_written += result.written;
            summary.bytes_missing += result.missing;

            if let Some(f) = &mut self.file_done {
                f(&result);
            }
            results.push(result);

            self.emit_progress(&mut last_progress, item, &summary, started, false);
        }

        summary.elapsed = started.elapsed();
        self.emit_last_progress(&summary, started);

        let mut warnings = self.source.warnings();
        if summary.cancelled {
            warnings.push("中断されたので、残りの項目はコピーしていない".to_string());
        }

        Ok(CopyReport {
            source: self.source.label(),
            destination: self.dest.clone(),
            summary,
            files: results,
            warnings,
        })
    }

    /// 1 ファイルをコピーする。失敗しても `Err` は返さず結果に記録する。
    fn copy_one(&self, item: &CopyItem) -> FileResult {
        let planned = self.output_path(item);
        if let Some(parent) = planned.parent()
            && let Err(e) = extract::create_dir(parent)
        {
            return failed(item, planned.clone(), e.to_string());
        }

        let output = match self.resolve_existing(&planned) {
            Some(path) => path,
            None => {
                return FileResult {
                    source: item.path.clone(),
                    output: planned,
                    size: item.size,
                    written: 0,
                    missing: 0,
                    read_errors: 0,
                    status: CopyStatus::Skipped,
                    error: None,
                };
            }
        };
        match self.source.copy_file(item, &output, &self.options) {
            Ok(stats) => FileResult {
                source: item.path.clone(),
                output,
                size: item.size,
                written: stats.written,
                missing: stats.missing,
                read_errors: stats.read_errors,
                // 欠けなしと言えるのは、埋めた所が無く、記録どおりの長さを
                // 書けたときだけ。読んでいる最中に短くなったファイルもここで
                // 「一部欠け」に落ちる。
                status: if stats.missing == 0 && stats.written == item.size {
                    CopyStatus::Copied
                } else {
                    CopyStatus::Partial
                },
                error: None,
            },
            Err(e) => {
                // 途中まで書けていた場合、そのファイルは中途半端に残る。
                // 消さずに残し、レポートで失敗と伝える(消すと救えた分まで失う)。
                let written = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
                let mut result = failed(item, output, e.to_string());
                result.written = written;
                result.missing = item.size.saturating_sub(written);
                result
            }
        }
    }

    /// 宛先に同名のファイルがあったときの行き先を決める。`None` なら飛ばす。
    fn resolve_existing(&self, planned: &Path) -> Option<PathBuf> {
        if !planned.exists() {
            return Some(planned.to_path_buf());
        }
        match self.options.on_existing {
            ExistingFile::Overwrite => Some(planned.to_path_buf()),
            ExistingFile::Skip => None,
            ExistingFile::Rename => {
                let dir = planned.parent().unwrap_or(&self.dest);
                let name = planned
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "_".to_string());
                Some(extract::unique_path(dir, &name))
            }
        }
    }

    fn output_path(&self, item: &CopyItem) -> PathBuf {
        let mut path = self.dest.clone();
        for component in &item.components {
            path.push(component);
        }
        path
    }

    fn emit_progress(
        &mut self,
        last: &mut Option<Instant>,
        item: &CopyItem,
        summary: &CopySummary,
        started: Instant,
        force: bool,
    ) {
        let Some(callback) = &mut self.progress else {
            return;
        };
        let now = Instant::now();
        if !force
            && let Some(previous) = *last
            && now.duration_since(previous) < self.options.progress_interval
        {
            return;
        }
        *last = Some(now);
        callback(&progress(item.path.clone(), summary, started));
    }

    fn emit_last_progress(&mut self, summary: &CopySummary, started: Instant) {
        if let Some(callback) = &mut self.progress {
            callback(&progress(String::new(), summary, started));
        }
    }
}

fn progress(current: String, summary: &CopySummary, started: Instant) -> CopyProgress {
    let elapsed = started.elapsed();
    let done = summary.copied + summary.partial + summary.failed + summary.skipped;
    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        (summary.bytes_written as f64 / secs) as u64
    } else {
        0
    };
    let remaining = summary.bytes_expected.saturating_sub(summary.bytes_written);
    CopyProgress {
        current,
        files_done: done,
        files_total: summary.files,
        bytes_done: summary.bytes_written,
        bytes_total: summary.bytes_expected,
        bytes_missing: summary.bytes_missing,
        failed: summary.failed,
        elapsed,
        rate,
        eta: (rate > 0).then(|| Duration::from_secs_f64(remaining as f64 / rate as f64)),
    }
}

fn failed(item: &CopyItem, output: PathBuf, error: String) -> FileResult {
    FileResult {
        source: item.path.clone(),
        output,
        size: item.size,
        written: 0,
        missing: item.size,
        read_errors: 0,
        status: CopyStatus::Failed,
        error: Some(error),
    }
}
