//! `ofr carve`: シグネチャからファイルを探して切り出す。

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ofr_carve::{CarveOptions, CarveReport, CarvedFile, Carver, FileFormat};

use crate::Outcome;
use crate::format;
use crate::source::{
    check_destination, check_source_selectable, install_cancel_handler, open_source, parse_size,
};

/// `ofr carve` の引数。
#[derive(Debug, clap::Args)]
pub struct CarveArgs {
    /// 復旧元。デバイス ID(`/dev/disk4`, `\\.\PhysicalDrive2`)か、取得済みイメージ。
    pub source: String,

    /// 切り出したファイルを置くディレクトリ。形式ごとのサブフォルダに分けて書く。
    pub output: PathBuf,

    /// 探す形式をカンマ区切りで指定する(例 `jpeg,png,mp4`)。既定は全形式。
    #[arg(short, long, value_delimiter = ',')]
    pub formats: Vec<String>,

    /// ファイル先頭を探す境界。FS のクラスタサイズが分かっているなら指定する。
    #[arg(short, long, default_value = "512")]
    pub align: String,

    /// 1 ファイルの最大サイズ。
    #[arg(long, default_value = "4G")]
    pub max_size: String,

    /// これより小さい切り出しは捨てる。
    #[arg(long, default_value = "64")]
    pub min_size: String,

    /// 走査を始める位置。
    #[arg(long)]
    pub start: Option<String>,

    /// 走査を終える位置。
    #[arg(long)]
    pub end: Option<String>,

    /// 終端を確定できなかったファイルは出力しない。
    #[arg(long)]
    pub skip_truncated: bool,

    /// 書き出さずに、見つかるファイルの一覧だけを出す。
    #[arg(long)]
    pub dry_run: bool,

    /// JSON レポートの出力先。既定は `<出力>/carve-report.json`。
    #[arg(long)]
    pub report: Option<PathBuf>,

    /// 開始前にデバイスをアンマウントする(macOS の `diskutil unmountDisk`)。
    #[arg(long)]
    pub unmount: bool,
}

/// カービングを実行する。
pub fn run(args: CarveArgs) -> Result<Outcome, Box<dyn std::error::Error>> {
    check_source_selectable(&args.source)?;

    let device = open_source(&args.source)?;
    let info = device.info().clone();
    if info.is_system_disk {
        return Err(format!("{} は起動ディスクなので復旧元にできない", info.id).into());
    }
    if !args.dry_run {
        check_destination(&info.id, &args.output)?;
    }

    if args.unmount {
        println!("{} をアンマウントする...", info.id);
        ofr_device::unmount_device(&info.id)?;
    }

    let options = build_options(&args)?;
    let tty = std::io::stderr().is_terminal();

    println!(
        "復旧元: {} ({}, {})",
        info.id,
        info.display_name,
        format::bytes(device.len())
    );
    if args.dry_run {
        println!("出力:   なし(--dry-run)");
    } else {
        println!("出力:   {}", args.output.display());
    }
    println!(
        "対象:   {}",
        match &options.formats {
            Some(list) => list.iter().map(|f| f.name()).collect::<Vec<_>>().join(", "),
            None => "全形式".to_string(),
        }
    );
    println!("境界:   {} バイトごと", options.align);
    println!();

    // PLAN.md 6章 4項: 壊れかけメディアは読むたびに劣化する。
    if info.kind != ofr_device::DeviceKind::ImageFile {
        println!("注意: デバイスを直接走査する。読み出しが不安定なメディアなら、");
        println!("      先に `ofr image` でイメージを取り、そのイメージを走査すること。");
        println!();
    }

    let cancel = Arc::new(AtomicBool::new(false));
    install_cancel_handler(
        Arc::clone(&cancel),
        "中断する。ここまでに切り出したファイルは残る。",
    );

    let dest = (!args.dry_run).then_some(args.output.as_path());
    let report = Carver::new(device.as_ref())
        .with_options(options)
        .with_cancel(cancel)
        .with_progress(progress_reporter(tty))
        .run(dest)?;

    if tty {
        eprintln!();
    }
    if args.dry_run {
        print_files(&report);
    }
    print_summary(&report, &args);

    let report_path = args
        .report
        .clone()
        .or_else(|| (!args.dry_run).then(|| args.output.join("carve-report.json")));
    if let Some(path) = report_path {
        write_report(&path, &args.source, &report)?;
        println!("レポート: {}", path.display());
    }

    Ok(if report.summary.cancelled {
        Outcome::Incomplete
    } else {
        Outcome::Complete
    })
}

fn build_options(args: &CarveArgs) -> Result<CarveOptions, Box<dyn std::error::Error>> {
    let formats = if args.formats.is_empty() {
        None
    } else {
        let mut list = Vec::new();
        for name in &args.formats {
            let f = FileFormat::from_name(name).ok_or_else(|| {
                format!(
                    "知らない形式: {name}(使えるのは {})",
                    FileFormat::all()
                        .iter()
                        .map(|f| f.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            if !list.contains(&f) {
                list.push(f);
            }
        }
        Some(list)
    };

    Ok(CarveOptions {
        align: parse_size(&args.align)?,
        formats,
        max_file_size: parse_size(&args.max_size)?,
        min_file_size: parse_size(&args.min_size)?,
        start_offset: args
            .start
            .as_deref()
            .map(parse_size)
            .transpose()?
            .unwrap_or(0),
        end_offset: args.end.as_deref().map(parse_size).transpose()?,
        include_truncated: !args.skip_truncated,
        ..CarveOptions::default()
    })
}

/// 進捗表示。端末なら 1 行を書き換え、そうでなければ 1 行ずつ追記する。
fn progress_reporter(tty: bool) -> impl FnMut(&ofr_carve::CarveProgress) + Send + 'static {
    let mut last_width = 0usize;
    move |p| {
        let line = format!(
            "走査 {} / {} ({:5.1}%)  速度 {}  残り {}  発見 {} 件 ({})  エラー {}",
            format::bytes(p.position.saturating_sub(p.start)),
            format::bytes(p.end.saturating_sub(p.start)),
            p.ratio() * 100.0,
            format::rate(p.rate),
            format::eta(p.eta),
            p.found,
            format::bytes(p.bytes_recovered),
            p.read_errors,
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

fn print_files(report: &CarveReport) {
    for f in &report.files {
        let meta = f.metadata.summary();
        println!(
            "{:>12}  {:>10}  {:<6} {:<4}  {}{}",
            f.offset,
            format::bytes(f.size),
            f.format.name(),
            f.confidence.label(),
            f.file_name,
            if meta.is_empty() {
                String::new()
            } else {
                format!("  [{meta}]")
            }
        );
    }
    if !report.files.is_empty() {
        println!();
    }
}

fn print_summary(report: &CarveReport, args: &CarveArgs) {
    let s = &report.summary;
    println!("---");
    println!("走査:   {}", format::bytes(s.scanned));
    println!(
        "発見:   {} 件 ({})",
        s.found,
        format::bytes(s.bytes_recovered)
    );

    for (fmt, n) in report.counts_by_format() {
        let bytes: u64 = report
            .files
            .iter()
            .filter(|f| f.format == fmt)
            .map(|f| f.size)
            .sum();
        println!(
            "  {} {:>5} 件  {:>10}  ({})",
            format::pad(fmt.name(), 6),
            n,
            format::bytes(bytes),
            fmt.category()
        );
    }

    let truncated = s.found - s.exact;
    if truncated > 0 {
        println!("境界推定: {truncated} 件(終端を確定できず、次のシグネチャの手前で切った)");
    }
    if s.read_errors > 0 {
        println!(
            "エラー: {} 回(欠けたバイト {})",
            s.read_errors,
            format::bytes(s.bad_bytes)
        );
    }
    println!("所要:   {}", format::duration(s.elapsed));

    if s.cancelled {
        println!();
        println!("中断した。--start で続きの位置から再開できる。");
    }
    if !args.dry_run && s.found > 0 {
        println!();
        println!("切り出したファイルは元の名前を持たない。中身を確認してから整理すること。");
    }
}

/// 結果を JSON で書き出す。GUI やスクリプトからはこれを読む。
fn write_report(
    path: &std::path::Path,
    source: &str,
    report: &CarveReport,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let s = &report.summary;
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"source\": {},\n", format::json_string(source)));
    out.push_str("  \"summary\": {\n");
    out.push_str(&format!("    \"scanned\": {},\n", s.scanned));
    out.push_str(&format!("    \"found\": {},\n", s.found));
    out.push_str(&format!("    \"exact\": {},\n", s.exact));
    out.push_str(&format!(
        "    \"bytes_recovered\": {},\n",
        s.bytes_recovered
    ));
    out.push_str(&format!("    \"read_errors\": {},\n", s.read_errors));
    out.push_str(&format!("    \"bad_bytes\": {},\n", s.bad_bytes));
    out.push_str(&format!("    \"elapsed_ms\": {},\n", s.elapsed.as_millis()));
    out.push_str(&format!("    \"cancelled\": {}\n", s.cancelled));
    out.push_str("  },\n");
    out.push_str("  \"files\": [\n");
    for (i, f) in report.files.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&file_json(f));
        out.push_str(if i + 1 == report.files.len() {
            "\n"
        } else {
            ",\n"
        });
    }
    out.push_str("  ]\n}\n");
    std::fs::write(path, out)?;
    Ok(())
}

fn file_json(f: &CarvedFile) -> String {
    let m = &f.metadata;
    let mut meta: Vec<String> = Vec::new();
    if let Some(ts) = m.timestamp {
        meta.push(format!(
            "\"timestamp\": {}",
            format::json_string(&ts.to_string())
        ));
    }
    if let Some(w) = m.width {
        meta.push(format!("\"width\": {w}"));
    }
    if let Some(h) = m.height {
        meta.push(format!("\"height\": {h}"));
    }
    if let Some(d) = m.duration_ms {
        meta.push(format!("\"duration_ms\": {d}"));
    }
    if let Some(make) = &m.camera_make {
        meta.push(format!("\"camera_make\": {}", format::json_string(make)));
    }
    if let Some(model) = &m.camera_model {
        meta.push(format!("\"camera_model\": {}", format::json_string(model)));
    }
    if let Some(o) = m.orientation {
        meta.push(format!("\"orientation\": {o}"));
    }

    format!(
        "{{\"index\": {}, \"name\": {}, \"format\": {}, \"extension\": {}, \
         \"offset\": {}, \"size\": {}, \"confidence\": {}, \"bad_bytes\": {}, \
         \"metadata\": {{{}}}}}",
        f.index,
        format::json_string(&f.file_name),
        format::json_string(f.format.name()),
        format::json_string(f.extension),
        f.offset,
        f.size,
        format::json_string(if f.confidence.is_exact() {
            "exact"
        } else {
            "truncated"
        }),
        f.bad_bytes,
        meta.join(", "),
    )
}
