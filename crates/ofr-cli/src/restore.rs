//! `ofr restore`: 見つかった項目を復元先へ書き出す。
//!
//! フォルダ構造はそのまま作る。名前は出力先の OS が受け付ける形に直し、
//! 同名のものがあれば `名前 (2).jpg` のように番号を足す(削除済みファイルには
//! 同名のものが普通にあるため)。

use std::error::Error;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ofr_device::{Device, SliceDevice};
use ofr_fs::{EntryStatus, ExtractOptions, ExtractStats, RecoveredEntry, ScanOptions, extract};

use crate::Outcome;
use crate::filter::{Filter, StatusChoice};
use crate::format;
use crate::scan::concerns;
use crate::source::{self, FsChoice};

/// レポートの既定のファイル名。
const REPORT_NAME: &str = "ofr-restore-report.json";

/// `ofr restore` の引数。
#[derive(Debug, clap::Args)]
pub struct RestoreArgs {
    /// 復旧元。デバイス ID か、`ofr image` で取ったイメージファイル。
    pub source: String,

    /// 復元先のフォルダ。復旧元と同じデバイス上は指定できない。
    pub dest: PathBuf,

    /// ファイルシステムを指定する(既定は自動判定)。
    #[arg(long, value_enum, default_value_t = FsChoice::Auto)]
    pub fs: FsChoice,

    /// ボリュームの開始位置を直接指定する。
    #[arg(long)]
    pub offset: Option<String>,

    /// 名前かパスで絞る(`--include '*.jpg'`)。複数指定できる。
    #[arg(long)]
    pub include: Vec<String>,

    /// 状態で絞る(`--status deleted`)。
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<StatusChoice>,

    /// 削除済みの項目を探さない。
    #[arg(long)]
    pub no_deleted: bool,

    /// 孤立クラスタ走査を省く。
    #[arg(long)]
    pub no_orphans: bool,

    /// フォルダ構造を作らず、復元先に平らに並べる。
    #[arg(long)]
    pub flatten: bool,

    /// 書き出さずに、何が復元されるかだけ表示する。
    #[arg(long)]
    pub dry_run: bool,

    /// 読めなかった部分をゼロで埋めずに、そのファイルを失敗扱いにする。
    #[arg(long)]
    pub no_zero_fill: bool,

    /// 読み込み失敗時のリトライ回数。
    #[arg(short = 'r', long, default_value_t = 2)]
    pub retries: u32,

    /// JSON レポートの出力先。既定は復元先の `ofr-restore-report.json`。
    #[arg(long)]
    pub report: Option<PathBuf>,

    /// レポートを書かない。
    #[arg(long, conflicts_with = "report")]
    pub no_report: bool,
}

/// 1 ファイルの復元結果。
struct Restored {
    path: String,
    output: PathBuf,
    size: u64,
    stats: ExtractStats,
    status: EntryStatus,
    error: Option<String>,
}

/// 復元を実行する。
pub fn run(args: RestoreArgs) -> Result<Outcome, Box<dyn Error>> {
    source::check_source_selectable(&args.source)?;
    let device = source::open_source(&args.source)?;
    let info = device.info().clone();

    // 6章 2項: 復元先が復旧元と同じデバイス上にあってはいけない。
    source::check_destination(&info.id, &args.dest)?;

    let offset = match &args.offset {
        Some(text) => Some(source::parse_size(text)?),
        None => None,
    };
    let volume = source::locate(device.as_ref(), args.fs, offset)?;
    let region = SliceDevice::new(device.as_ref(), volume.offset, volume.len)?;
    let fs = source::open_filesystem(&region, volume.kind)?;

    println!(
        "復旧元: {} ({})  ボリューム: {}",
        info.id,
        format::bytes(device.len()),
        fs.volume().kind
    );
    println!("復元先: {}", args.dest.display());
    println!();

    let cancel = Arc::new(AtomicBool::new(false));
    source::install_cancel_handler(
        Arc::clone(&cancel),
        "中断する。ここまでに書き出したファイルは残る。",
    );

    let options = ScanOptions {
        deleted: !args.no_deleted,
        orphans: !args.no_orphans,
        cancel: Arc::clone(&cancel),
        ..ScanOptions::default()
    };
    println!("走査中...");
    let tree = fs.scan(&options, None)?;

    let filter = Filter::new(args.include.clone(), &args.status);
    let targets: Vec<&RecoveredEntry> = tree
        .entries()
        .iter()
        // 中身が空のファイルも復元する(0 バイトのファイルを作る)。
        .filter(|e| !e.is_dir() && filter.matches(e))
        .collect();

    let total_bytes: u64 = targets.iter().map(|e| e.recoverable_bytes()).sum();
    println!(
        "対象: {} 件 ({})",
        targets.len(),
        format::bytes(total_bytes)
    );
    if targets.is_empty() {
        println!("復元できる項目がない。`ofr scan` で何が見つかるか確かめること。");
        return Ok(Outcome::Incomplete);
    }
    if args.dry_run {
        for entry in &targets {
            let output = output_path(&args.dest, entry, args.flatten);
            println!(
                "  {} → {}{}",
                entry.path,
                output.strip_prefix(&args.dest).unwrap_or(&output).display(),
                concerns(entry)
            );
        }
        println!();
        println!("--dry-run なので何も書き出していない。");
        return Ok(Outcome::Complete);
    }

    extract::create_dir(&args.dest)?;
    let extract_options = ExtractOptions {
        retries: args.retries,
        zero_fill: !args.no_zero_fill,
        ..ExtractOptions::default()
    };

    let mut results = Vec::with_capacity(targets.len());
    for (i, entry) in targets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            println!("中断した。");
            break;
        }
        let output = output_path(&args.dest, entry, args.flatten);
        if let Some(parent) = output.parent() {
            extract::create_dir(parent)?;
        }
        // 同名のものがあれば番号を足す。
        let output = extract::unique_path(
            output.parent().unwrap_or(&args.dest),
            &output
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "_".to_string()),
        );

        let result = extract::extract_to_path(&region, entry, &output, &extract_options);
        let (stats, error) = match result {
            Ok(stats) => (stats, None),
            Err(e) => (ExtractStats::default(), Some(e.to_string())),
        };

        println!(
            "[{}/{}] {} → {} ({}){}{}",
            i + 1,
            targets.len(),
            entry.path,
            // 復元先からの相対パスで出す。絶対パスは長くて読みにくい。
            output.strip_prefix(&args.dest).unwrap_or(&output).display(),
            format::bytes(stats.written),
            if stats.missing > 0 {
                format!("  ※ {} 読めず", format::bytes(stats.missing))
            } else {
                String::new()
            },
            match &error {
                Some(e) => format!("  ※ 失敗: {e}"),
                None => String::new(),
            }
        );

        results.push(Restored {
            path: entry.path.clone(),
            output,
            size: entry.size,
            stats,
            status: entry.status,
            error,
        });
    }

    let report_path = if args.no_report {
        None
    } else {
        Some(
            args.report
                .clone()
                .unwrap_or_else(|| args.dest.join(REPORT_NAME)),
        )
    };
    if let Some(path) = &report_path
        && let Err(e) = write_report(path, &results)
    {
        eprintln!("レポートを書けなかった: {e}");
    }

    print_summary(&results, report_path.as_deref());

    let complete = results
        .iter()
        .all(|r| r.error.is_none() && r.stats.is_complete())
        && results.len() == targets.len();
    Ok(if complete {
        Outcome::Complete
    } else {
        Outcome::Incomplete
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

fn print_summary(results: &[Restored], report: Option<&Path>) {
    let written: u64 = results.iter().map(|r| r.stats.written).sum();
    let missing: u64 = results.iter().map(|r| r.stats.missing).sum();
    let failed = results.iter().filter(|r| r.error.is_some()).count();
    let partial = results
        .iter()
        .filter(|r| r.error.is_none() && !r.stats.is_complete())
        .count();

    println!("---");
    println!(
        "復元: {} 件 / {}",
        results.len() - failed,
        format::bytes(written)
    );
    if missing > 0 {
        println!(
            "読めずにゼロで埋めた部分: {} ({} 件)",
            format::bytes(missing),
            partial
        );
    }
    if failed > 0 {
        println!("失敗: {failed} 件");
    }
    if let Some(path) = report {
        println!("レポート: {}", path.display());
    }
    println!();
    println!(
        "復元したファイルが開けない場合、断片化していた可能性がある\
         (`ofr repair` で直せることがある)。"
    );
}

fn write_report(path: &Path, results: &[Restored]) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut out = BufWriter::new(file);

    writeln!(out, "{{")?;
    writeln!(out, "  \"files\": [")?;
    for (i, r) in results.iter().enumerate() {
        let comma = if i + 1 == results.len() { "" } else { "," };
        writeln!(
            out,
            concat!(
                "    {{\"path\": {}, \"output\": {}, \"size\": {}, \"written\": {}, ",
                "\"missing\": {}, \"read_errors\": {}, \"status\": {}, \"error\": {}}}{}"
            ),
            format::json_string(&r.path),
            format::json_string(&r.output.display().to_string()),
            r.size,
            r.stats.written,
            r.stats.missing,
            r.stats.read_errors,
            format::json_string(r.status.as_str()),
            match &r.error {
                Some(e) => format::json_string(e),
                None => "null".to_string(),
            },
            comma
        )?;
    }
    writeln!(out, "  ],")?;
    writeln!(
        out,
        concat!(
            "  \"summary\": {{\"files\": {}, \"written\": {}, \"missing\": {}, ",
            "\"failed\": {}}}"
        ),
        results.len(),
        results.iter().map(|r| r.stats.written).sum::<u64>(),
        results.iter().map(|r| r.stats.missing).sum::<u64>(),
        results.iter().filter(|r| r.error.is_some()).count(),
    )?;
    writeln!(out, "}}")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use ofr_fs::{EntryKind, EntryStatus, RecoveredEntry};

    use super::*;

    fn entry(path: &str) -> RecoveredEntry {
        let name = path.rsplit('/').next().unwrap_or(path);
        let mut e = RecoveredEntry::new(name, EntryKind::File, EntryStatus::Deleted);
        e.path = path.to_string();
        e
    }

    #[test]
    fn mirrors_the_directory_structure() {
        let dest = Path::new("/out");
        let path = output_path(dest, &entry("/DCIM/100MSDCF/a.jpg"), false);
        assert_eq!(path, Path::new("/out/DCIM/100MSDCF/a.jpg"));
    }

    #[test]
    fn flattens_when_asked() {
        let dest = Path::new("/out");
        let path = output_path(dest, &entry("/DCIM/100MSDCF/a.jpg"), true);
        assert_eq!(path, Path::new("/out/a.jpg"));
    }

    #[test]
    fn sanitizes_names_that_the_os_would_reject() {
        let dest = Path::new("/out");
        // Windows で使えない文字と予約名。
        let path = output_path(dest, &entry("/CON/a:b.txt"), false);
        assert_eq!(path, Path::new("/out/_CON/a_b.txt"));
    }
}
