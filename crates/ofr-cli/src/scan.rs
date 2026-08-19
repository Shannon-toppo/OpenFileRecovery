//! `ofr scan`: ファイルシステムを解析して、復元できる項目を一覧する。

use std::error::Error;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ofr_device::{Device, SliceDevice};
use ofr_fs::{
    EntryKind, EntryStatus, FileTree, RecoveredEntry, ScanOptions, ScanProgress, VolumeInfo,
};

use crate::filter::{Filter, StatusChoice};
use crate::format::{self, pad, pad_left};
use crate::source::{self, FsChoice};
use crate::{EXIT_HINT_EMPTY, Outcome};

/// `ofr scan` の引数。
#[derive(Debug, clap::Args)]
pub struct ScanArgs {
    /// 復旧元。デバイス ID(`/dev/disk4`)か、`ofr image` で取ったイメージファイル。
    pub source: String,

    /// ファイルシステムを指定する(既定は自動判定)。
    #[arg(long, value_enum, default_value_t = FsChoice::Auto)]
    pub fs: FsChoice,

    /// ボリュームの開始位置を直接指定する(`1M` のような接尾辞可)。
    #[arg(long)]
    pub offset: Option<String>,

    /// 削除済みの項目を探さない。
    #[arg(long)]
    pub no_deleted: bool,

    /// 孤立クラスタ走査を省く。速いが、フォーマット後のデバイスでは何も出ない。
    #[arg(long)]
    pub no_orphans: bool,

    /// 状態で絞る(`--status deleted,orphaned`)。
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<StatusChoice>,

    /// 名前かパスで絞る(`--include '*.jpg'`)。複数指定できる。
    #[arg(long)]
    pub include: Vec<String>,

    /// ツリーの形で表示する。
    #[arg(long)]
    pub tree: bool,

    /// JSON で出力する。
    #[arg(long)]
    pub json: bool,

    /// 表示する件数の上限。0 で無制限。
    #[arg(long, default_value_t = 200)]
    pub limit: usize,
}

/// 走査を実行する。
pub fn run(args: ScanArgs) -> Result<Outcome, Box<dyn Error>> {
    source::check_source_selectable(&args.source)?;
    let device = source::open_source(&args.source)?;
    let info = device.info().clone();

    let offset = match &args.offset {
        Some(text) => Some(source::parse_size(text)?),
        None => None,
    };
    let volume = source::locate(device.as_ref(), args.fs, offset)?;
    let region = SliceDevice::new(device.as_ref(), volume.offset, volume.len)?;
    let fs = source::open_filesystem(&region, volume.kind)?;

    if !args.json {
        println!(
            "復旧元: {} ({}, {})",
            info.id,
            info.display_name,
            format::bytes(device.len())
        );
        print_volume(fs.volume(), volume.offset, &volume.partition.type_name);
        println!();
    }

    let tty = std::io::stderr().is_terminal();
    let cancel = Arc::new(AtomicBool::new(false));
    source::install_cancel_handler(Arc::clone(&cancel), "中断する。ここまでの結果を表示する。");

    let options = ScanOptions {
        deleted: !args.no_deleted,
        orphans: !args.no_orphans,
        cancel,
        progress_interval: if tty {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        },
        ..ScanOptions::default()
    };

    let progress =
        (!args.json).then(|| -> ofr_fs::ScanProgressFn { Box::new(progress_reporter(tty)) });
    let tree = fs.scan(&options, progress)?;
    if tty && !args.json {
        eprintln!();
    }

    let filter = Filter::new(args.include.clone(), &args.status);
    let selected: Vec<&RecoveredEntry> = tree
        .entries()
        .iter()
        .filter(|e| filter.matches(e))
        .collect();

    if args.json {
        print_json(fs.volume(), &tree, &selected);
    } else if args.tree {
        print_tree(&tree, &filter, args.limit);
        print_summary(&tree, &selected, &filter);
    } else {
        print_list(&selected, args.limit);
        print_summary(&tree, &selected, &filter);
    }

    Ok(if selected.is_empty() {
        Outcome::Incomplete
    } else {
        Outcome::Complete
    })
}

fn print_volume(volume: &VolumeInfo, offset: u64, partition: &str) {
    println!(
        "ボリューム: {} {}  クラスタ {}  容量 {}",
        volume.kind,
        volume
            .label
            .as_deref()
            .map(|l| format!("\"{l}\""))
            .unwrap_or_else(|| "(ラベルなし)".to_string()),
        format::bytes(volume.bytes_per_cluster as u64),
        format::bytes(volume.total_bytes),
    );
    println!(
        "位置:       デバイス先頭から {} ({})",
        format::bytes(offset),
        partition,
    );
    println!("ジオメトリ: {} から読んだ", volume.boot_source.label());
    for note in &volume.notes {
        println!("注意:       {note}");
    }
}

fn print_list(entries: &[&RecoveredEntry], limit: usize) {
    let files: Vec<&&RecoveredEntry> = entries.iter().filter(|e| !e.is_dir()).collect();
    if files.is_empty() {
        return;
    }

    const STATUS_WIDTH: usize = 10;
    const SIZE_WIDTH: usize = 10;
    const TIME_WIDTH: usize = 19;
    println!(
        "{}  {}  {}  パス",
        pad("状態", STATUS_WIDTH),
        pad_left("サイズ", SIZE_WIDTH),
        pad("更新日時", TIME_WIDTH),
    );

    let shown = if limit == 0 { files.len() } else { limit };
    for entry in files.iter().take(shown) {
        println!(
            "{}  {}  {}  {}{}",
            pad(entry.status.label(), STATUS_WIDTH),
            pad_left(&format::bytes(entry.size), SIZE_WIDTH),
            pad(&timestamp(entry), TIME_WIDTH),
            entry.path,
            concerns(entry),
        );
    }
    if files.len() > shown {
        println!("... 他 {} 件(--limit 0 で全部出す)", files.len() - shown);
    }
}

fn print_tree(tree: &FileTree, filter: &Filter, limit: usize) {
    let rows = tree.depth_first();
    let shown = if limit == 0 { rows.len() } else { limit };
    let mut count = 0;

    for (depth, id) in rows {
        let Some(entry) = tree.get(id) else { continue };
        // ディレクトリは、絞り込みに関係なく骨組みとして出す。
        if !entry.is_dir() && !filter.matches(entry) {
            continue;
        }
        if count >= shown {
            println!("...");
            break;
        }
        count += 1;
        let indent = "  ".repeat(depth);
        let suffix = if entry.is_dir() { "/" } else { "" };
        let size = if entry.is_dir() {
            String::new()
        } else {
            format!("  {}", format::bytes(entry.size))
        };
        println!(
            "{indent}{}{suffix}  [{}]{size}{}",
            entry.name,
            entry.status.label(),
            concerns(entry)
        );
    }
}

fn print_summary(tree: &FileTree, selected: &[&RecoveredEntry], filter: &Filter) {
    let stats = &tree.stats;
    println!();
    if !filter.is_empty() {
        println!(
            "絞り込み後: {} 件(全体 {} 件)",
            selected.iter().filter(|e| !e.is_dir()).count(),
            stats.files
        );
    }
    println!(
        "見つかった項目: ファイル {} / ディレクトリ {}",
        stats.files, stats.dirs
    );
    println!(
        "状態の内訳(ディレクトリを含む): 無傷 {} / 削除済み {} / 孤立 {} / 破損 {}",
        tree.entries()
            .iter()
            .filter(|e| e.status == EntryStatus::Intact)
            .count(),
        stats.deleted,
        stats.orphaned,
        stats.damaged,
    );
    println!(
        "走査クラスタ: {}  所要 {}",
        stats.clusters_scanned,
        format::duration(stats.elapsed)
    );
    if stats.cancelled {
        println!("中断したので、これが全部とは限らない。");
    }
    if stats.truncated {
        println!("項目数の上限に達したので打ち切った。");
    }
    for warning in tree.warnings.iter().take(5) {
        println!("警告: {warning}");
    }
    if tree.warnings.len() > 5 {
        println!("警告: 他 {} 件", tree.warnings.len() - 5);
    }
    if selected.is_empty() {
        println!("{EXIT_HINT_EMPTY}");
    } else {
        println!();
        println!("復元するには: ofr restore <復旧元> <復元先フォルダ>");
    }
}

fn print_json(volume: &VolumeInfo, tree: &FileTree, selected: &[&RecoveredEntry]) {
    println!("{{");
    println!(
        concat!(
            "  \"volume\": {{\"type\": {}, \"label\": {}, \"cluster_size\": {}, ",
            "\"total_bytes\": {}, \"boot_source\": {}}},"
        ),
        format::json_string(volume.kind.label()),
        match &volume.label {
            Some(l) => format::json_string(l),
            None => "null".to_string(),
        },
        volume.bytes_per_cluster,
        volume.total_bytes,
        format::json_string(volume.boot_source.label()),
    );
    println!(
        concat!(
            "  \"stats\": {{\"files\": {}, \"dirs\": {}, \"deleted\": {}, \"orphaned\": {}, ",
            "\"damaged\": {}, \"cancelled\": {}, \"elapsed_ms\": {}}},"
        ),
        tree.stats.files,
        tree.stats.dirs,
        tree.stats.deleted,
        tree.stats.orphaned,
        tree.stats.damaged,
        tree.stats.cancelled,
        tree.stats.elapsed.as_millis(),
    );
    println!("  \"entries\": [");
    for (i, entry) in selected.iter().enumerate() {
        let comma = if i + 1 == selected.len() { "" } else { "," };
        println!(
            concat!(
                "    {{\"path\": {}, \"kind\": {}, \"size\": {}, \"status\": {}, ",
                "\"modified\": {}, \"first_cluster\": {}, \"recoverable_bytes\": {}, ",
                "\"contiguous_assumed\": {}, \"name_partial\": {}, ",
                "\"conflicting_clusters\": {}, \"truncated\": {}}}{}"
            ),
            format::json_string(&entry.path),
            format::json_string(if entry.kind == EntryKind::Dir {
                "dir"
            } else {
                "file"
            }),
            entry.size,
            format::json_string(entry.status.as_str()),
            match entry.times.modified {
                Some(t) => format::json_string(&t.to_string()),
                None => "null".to_string(),
            },
            match entry.first_cluster {
                Some(c) => c.to_string(),
                None => "null".to_string(),
            },
            entry.recoverable_bytes(),
            entry.quality.contiguous_assumed,
            entry.quality.name_partial,
            entry.quality.conflicting_clusters,
            entry.quality.truncated,
            comma
        );
    }
    println!("  ]");
    println!("}}");
}

fn timestamp(entry: &RecoveredEntry) -> String {
    entry
        .times
        .best()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// 項目に付ける注記。何を疑うべきかを 1 行で伝える。
pub fn concerns(entry: &RecoveredEntry) -> String {
    let mut notes = Vec::new();
    if entry.quality.contiguous_assumed {
        notes.push("連続配置と仮定".to_string());
    }
    if entry.quality.conflicting_clusters > 0 {
        notes.push(format!(
            "使用中クラスタ {} 個",
            entry.quality.conflicting_clusters
        ));
    }
    if entry.quality.name_partial {
        notes.push("名前が不完全".to_string());
    }
    if entry.quality.truncated {
        notes.push("領域が足りない".to_string());
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!("  ({})", notes.join(", "))
    }
}

fn progress_reporter(tty: bool) -> impl FnMut(&ScanProgress) + Send + 'static {
    let mut last_width = 0usize;
    move |p| {
        let percent = if p.total > 0 {
            p.position as f64 / p.total as f64 * 100.0
        } else {
            0.0
        };
        let line = format!(
            "{}  {} 件  {}",
            pad(p.phase.label(), 20),
            p.found,
            if p.position > 0 {
                format!("{percent:5.1}%")
            } else {
                format::duration(p.elapsed)
            }
        );
        let mut err = std::io::stderr();
        if tty {
            let width = format::display_width(&line);
            let _ = write!(
                err,
                "\r{line}{}",
                " ".repeat(last_width.saturating_sub(width))
            );
            last_width = width;
        } else {
            let _ = writeln!(err, "{line}");
        }
        let _ = err.flush();
    }
}
