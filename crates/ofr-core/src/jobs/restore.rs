//! 復元ジョブ。解析セッションで選ばれた項目を復元先へ書き出す。
//!
//! フォルダ構造はそのまま作る。名前は出力先の OS が受け付ける形に直し、
//! 同名のものがあれば `名前 (2).jpg` のように番号を足す(削除済みファイルには
//! 同名のものが普通にあるため)。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ofr_fs::{ExtractOptions, ExtractStats, RecoveredEntry, extract};
use serde::Serialize;

use super::JobCtx;
use crate::dto::{CopySummaryDto, FileResultDto, ProgressDto};
use crate::error::{CoreError, Result};
use crate::job::{CopyResultDto, ItemDto, JobEvent, JobResult, NoteLevel, Outcome, RestoreRequest};
use crate::session::Session;
use crate::source;

/// レポートに載せる 1 ファイルの記録。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportFile {
    path: String,
    output: String,
    size: u64,
    written: u64,
    missing: u64,
    read_errors: u32,
    status: &'static str,
    error: Option<String>,
}

/// レポート全体。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    source: String,
    destination: String,
    summary: CopySummaryDto,
    files: Vec<ReportFile>,
}

/// 復元を実行する。
pub(crate) fn run(ctx: &JobCtx, req: RestoreRequest) -> Result<(Outcome, JobResult)> {
    let session = ctx
        .core
        .session(req.session)
        .ok_or(CoreError::NoSession(req.session))?;
    let Session::Scan(scan) = &*session else {
        return Err(CoreError::BadRequest(
            "このセッションは解析結果ではありません".to_string(),
        ));
    };

    // 6章 2項: 復元先が復旧元と同じデバイス上にあってはいけない。
    source::check_destination(&scan.device.info().id, &req.dest)?;

    let targets = scan.expand(&req.entries);
    if targets.is_empty() {
        return Err(CoreError::BadRequest(
            "復元する項目が選択されていません".to_string(),
        ));
    }

    let region = scan.region()?;
    let options = ExtractOptions {
        retries: req.retries,
        zero_fill: req.zero_fill,
        ..ExtractOptions::default()
    };

    extract::create_dir(&req.dest).map_err(|e| CoreError::Io {
        path: req.dest.clone(),
        source: std::io::Error::other(e.to_string()),
    })?;

    let bytes_total: u64 = targets
        .iter()
        .filter_map(|id| scan.tree.get(*id))
        .map(RecoveredEntry::recoverable_bytes)
        .sum();

    let started = Instant::now();
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    let mut summary = CopySummaryDto {
        files: targets.len() as u64,
        destination: req.dest.display().to_string(),
        ..CopySummaryDto::default()
    };
    let mut files: Vec<ReportFile> = Vec::with_capacity(targets.len());
    let mut incomplete: Vec<FileResultDto> = Vec::new();

    for (done, id) in targets.iter().enumerate() {
        if ctx.cancelled() {
            summary.cancelled = true;
            break;
        }
        let Some(entry) = scan.tree.get(*id) else {
            continue;
        };

        let output = output_path(&req.dest, entry, req.flatten);
        if let Some(parent) = output.parent() {
            extract::create_dir(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: std::io::Error::other(e.to_string()),
            })?;
        }
        let output = extract::unique_path(
            output.parent().unwrap_or(&req.dest),
            &output
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "_".to_string()),
        );

        let (stats, error) = match extract::extract_to_path(&region, entry, &output, &options) {
            Ok(stats) => (stats, None),
            Err(e) => (ExtractStats::default(), Some(e.to_string())),
        };
        let status = status_of(&stats, error.is_some());

        match status {
            "copied" => summary.copied += 1,
            "partial" => summary.partial += 1,
            _ => summary.failed += 1,
        }
        summary.bytes_written += stats.written;
        summary.bytes_missing += stats.missing;

        let result = FileResultDto {
            source: entry.path.clone(),
            output: output.display().to_string(),
            size: entry.size,
            written: stats.written,
            missing: stats.missing,
            status,
            error: error.clone(),
        };
        if status != "copied" && incomplete.len() < 200 {
            incomplete.push(result.clone());
        }
        ctx.emit(JobEvent::Item {
            job: ctx.id,
            item: ItemDto::File(Box::new(result)),
        });

        files.push(ReportFile {
            path: entry.path.clone(),
            output: output.display().to_string(),
            size: entry.size,
            written: stats.written,
            missing: stats.missing,
            read_errors: stats.read_errors,
            status,
            error,
        });

        // 進捗は 100ms 間隔に間引く(PLAN.md 5.7)。最後の 1 件は必ず出す。
        let last = done + 1 == targets.len();
        if last || last_progress.elapsed() >= Duration::from_millis(100) {
            last_progress = Instant::now();
            let elapsed = started.elapsed();
            let rate = rate(summary.bytes_written, elapsed);
            ctx.progress(ProgressDto {
                phase: "restoring",
                items_done: done as u64 + 1,
                items_total: targets.len() as u64,
                bytes_done: summary.bytes_written,
                bytes_total,
                ratio: if bytes_total > 0 {
                    summary.bytes_written as f64 / bytes_total as f64
                } else {
                    (done as f64 + 1.0) / targets.len() as f64
                },
                rate,
                eta_secs: (rate > 0)
                    .then(|| bytes_total.saturating_sub(summary.bytes_written) / rate),
                elapsed_secs: elapsed.as_secs_f64(),
                current: entry.path.clone(),
                ..ProgressDto::default()
            });
        }
    }

    summary.elapsed_secs = started.elapsed().as_secs_f64();
    summary.complete = !summary.cancelled && summary.failed == 0 && summary.partial == 0;

    let report_path = req.dest.join(crate::RESTORE_REPORT_NAME);
    let report = Report {
        source: scan.source.clone(),
        destination: req.dest.display().to_string(),
        summary: summary.clone(),
        files,
    };
    match write_report(&report_path, &report) {
        Ok(()) => summary.report_json = Some(report_path.display().to_string()),
        Err(e) => ctx.note(
            NoteLevel::Warn,
            format!("レポートを作成できませんでした: {}", e.full_message()),
        ),
    }

    if summary.partial > 0 || summary.failed > 0 {
        ctx.note(
            NoteLevel::Warn,
            "欠けたファイルは、断片化していたか領域が上書きされています。開けない場合は修復を試してください。",
        );
    }

    Ok((
        ctx.outcome(summary.complete),
        JobResult::Restore(CopyResultDto {
            summary,
            incomplete,
        }),
    ))
}

fn status_of(stats: &ExtractStats, failed: bool) -> &'static str {
    if failed {
        "failed"
    } else if stats.is_complete() {
        "copied"
    } else if stats.written > 0 {
        "partial"
    } else {
        "failed"
    }
}

fn rate(bytes: u64, elapsed: Duration) -> u64 {
    let secs = elapsed.as_secs_f64();
    if secs > 0.0 {
        (bytes as f64 / secs) as u64
    } else {
        0
    }
}

fn write_report(path: &Path, report: &Report) -> Result<()> {
    let json = serde_json::to_vec_pretty(report).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    std::fs::write(path, json).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// 復元先のパスを組み立てる。各要素は OS が受け付ける形に直す。
fn output_path(dest: &Path, entry: &RecoveredEntry, flatten: bool) -> PathBuf {
    let mut path = dest.to_path_buf();
    let components: Vec<&str> = entry.path.split('/').filter(|s| !s.is_empty()).collect();
    if flatten {
        if let Some(name) = components.last() {
            path.push(extract::sanitize_component(name));
        }
        return path;
    }
    for component in components {
        path.push(extract::sanitize_component(component));
    }
    path
}

#[cfg(test)]
mod tests {
    use ofr_fs::{EntryKind, EntryStatus};

    use super::*;

    fn entry(path: &str) -> RecoveredEntry {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut e = RecoveredEntry::new(name, EntryKind::File, EntryStatus::Deleted);
        e.path = path.to_string();
        e
    }

    #[test]
    fn mirrors_the_directory_structure() {
        let path = output_path(Path::new("/out"), &entry("/DCIM/100MSDCF/a.jpg"), false);
        assert_eq!(path, Path::new("/out/DCIM/100MSDCF/a.jpg"));
    }

    #[test]
    fn flattens_when_asked() {
        let path = output_path(Path::new("/out"), &entry("/DCIM/100MSDCF/a.jpg"), true);
        assert_eq!(path, Path::new("/out/a.jpg"));
    }

    #[test]
    fn sanitizes_names_that_the_os_would_reject() {
        let path = output_path(Path::new("/out"), &entry("/CON/a:b.txt"), false);
        assert_eq!(path, Path::new("/out/_CON/a_b.txt"));
    }
}
