//! `ofr copy`: デバイスの中身をフォルダ構造ごと宛先へ写す。
//!
//! 復旧元の指定で読み出し経路が決まる(PLAN.md 5.5):
//!
//! - フォルダ(マウント済みのデバイス)→ OS のファイル API で読む論理コピー
//! - デバイス ID / イメージファイル → FAT32 / exFAT を直読みして展開する
//!
//! どちらでも宛先にできるミラーツリーとレポートは同じ形になる。
//! 消えたファイルを選んで拾いたいときは `ofr copy` ではなく `ofr restore`。

use std::error::Error;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ofr_copy::{
    Copier, CopyOptions, CopyProgress, CopyReport, CopySource, ExistingFile, MountSource,
    REPORT_JSON, REPORT_TEXT, TreeSource,
};
use ofr_device::{Device, SliceDevice};
use ofr_fs::{EntryStatus, ScanOptions};

use crate::Outcome;
use crate::format;
use crate::source::{self, FsChoice};

/// `--on-existing` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ExistingChoice {
    /// `名前 (2).jpg` のように番号を足して両方残す。
    Rename,
    /// 飛ばす(中断したコピーの続きに使う)。
    Skip,
    /// 上書きする。
    Overwrite,
}

impl From<ExistingChoice> for ExistingFile {
    fn from(value: ExistingChoice) -> Self {
        match value {
            ExistingChoice::Rename => ExistingFile::Rename,
            ExistingChoice::Skip => ExistingFile::Skip,
            ExistingChoice::Overwrite => ExistingFile::Overwrite,
        }
    }
}

/// `ofr copy` の引数。
#[derive(Debug, clap::Args)]
pub struct CopyArgs {
    /// 復旧元。マウント済みのフォルダ、デバイス ID、または取得済みイメージ。
    pub source: String,

    /// 宛先フォルダ。復旧元と同じデバイス上は指定できない。
    pub dest: PathBuf,

    /// ファイルシステムを指定する(既定は自動判定)。デバイス / イメージのみ。
    #[arg(long, value_enum, default_value_t = FsChoice::Auto)]
    pub fs: FsChoice,

    /// ボリュームの開始位置を直接指定する。デバイス / イメージのみ。
    #[arg(long)]
    pub offset: Option<String>,

    /// 削除済み・孤立の項目もコピーする。デバイス / イメージのみ。
    #[arg(long)]
    pub include_deleted: bool,

    /// 宛先に同名のファイルがあったときの扱い。
    #[arg(long, value_enum, default_value = "rename")]
    pub on_existing: ExistingChoice,

    /// 空のフォルダは作らない。
    #[arg(long)]
    pub no_empty_dirs: bool,

    /// 読めなかった部分をゼロで埋めずに、そのファイルを失敗扱いにする。
    #[arg(long)]
    pub no_zero_fill: bool,

    /// 元のタイムスタンプを宛先に反映しない。
    #[arg(long)]
    pub no_timestamps: bool,

    /// 読み込み失敗時のリトライ回数。
    #[arg(short = 'r', long, default_value_t = 2)]
    pub retries: u32,

    /// 読み込み単位。壊れかけメディアでは小さくすると欠ける量が減る。
    #[arg(long, default_value = "1M")]
    pub chunk_size: String,

    /// 書き出さずに、何がコピーされるかだけ表示する。
    #[arg(long)]
    pub dry_run: bool,

    /// JSON レポートの出力先。既定は宛先の `ofr-copy-report.json`
    /// (人間向けサマリは同じ場所に `.txt` で並べて書く)。
    #[arg(long)]
    pub report: Option<PathBuf>,

    /// レポートを書かない。
    #[arg(long, conflicts_with = "report")]
    pub no_report: bool,

    /// 開始前にデバイスをアンマウントする(macOS の `diskutil unmountDisk`)。
    #[arg(long)]
    pub unmount: bool,
}

/// コピーを実行する。
pub fn run(args: CopyArgs) -> Result<Outcome, Box<dyn Error>> {
    if Path::new(&args.source).is_dir() {
        run_logical(&args)
    } else {
        run_raw(&args)
    }
}

/// マウント済みフォルダからの論理コピー。
fn run_logical(args: &CopyArgs) -> Result<Outcome, Box<dyn Error>> {
    let root = PathBuf::from(&args.source);
    // 6章 2項: 宛先が復旧元と同じデバイス上にあってはいけない。
    // (宛先が復旧元フォルダの中にある場合は ofr-copy 側が弾く。)
    if !args.dry_run
        && let Some(disk) = ofr_device::disk_id_for_path(&root)
    {
        source::check_destination(&disk, &args.dest)?;
    }

    println!("復旧元: {} (マウント済みフォルダ)", root.display());
    println!("宛先:   {}", args.dest.display());
    println!("経路:   OS のファイル API で読む論理コピー");
    println!();

    let source = MountSource::new(&root);
    copy(&source, args)
}

/// デバイス / イメージからの直読みコピー(イメージ展開)。
fn run_raw(args: &CopyArgs) -> Result<Outcome, Box<dyn Error>> {
    source::check_source_selectable(&args.source)?;
    let device = source::open_source(&args.source)?;
    let info = device.info().clone();
    if !args.dry_run {
        source::check_destination(&info.id, &args.dest)?;
    }

    if args.unmount {
        println!("{} をアンマウントする...", info.id);
        ofr_device::unmount_device(&info.id)?;
    }

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
    println!("宛先:   {}", args.dest.display());
    println!("経路:   ファイルシステムを直読みして展開");
    if info.kind != ofr_device::DeviceKind::ImageFile {
        // PLAN.md 6章 4項。
        println!();
        println!("注意: デバイスを直接読む。読み出しが不安定なメディアなら、");
        println!("      先に `ofr image` でイメージを取り、そのイメージをコピー元にすること。");
    }
    println!();

    let cancel = Arc::new(AtomicBool::new(false));
    source::install_cancel_handler(
        Arc::clone(&cancel),
        "中断する。ここまでに書き出したファイルは残る。",
    );

    println!("走査中...");
    let scan_options = ScanOptions {
        // コピーは「いま入っているもの」が対象。削除済みを足すときだけ
        // 孤立クラスタ走査まで回す(時間がかかるため)。
        deleted: args.include_deleted,
        orphans: args.include_deleted,
        cancel: Arc::clone(&cancel),
        ..ScanOptions::default()
    };
    let tree = fs.scan(&scan_options, None)?;

    let mut tree_source = TreeSource::new(&region, &tree).with_label(&args.source);
    if args.include_deleted {
        tree_source = tree_source.with_statuses([
            EntryStatus::Intact,
            EntryStatus::Deleted,
            EntryStatus::Orphaned,
            EntryStatus::Damaged,
        ]);
    }
    copy_with_cancel(&tree_source, args, cancel)
}

fn copy(source: &dyn CopySource, args: &CopyArgs) -> Result<Outcome, Box<dyn Error>> {
    let cancel = Arc::new(AtomicBool::new(false));
    source::install_cancel_handler(
        Arc::clone(&cancel),
        "中断する。ここまでに書き出したファイルは残る。",
    );
    copy_with_cancel(source, args, cancel)
}

fn copy_with_cancel(
    source: &dyn CopySource,
    args: &CopyArgs,
    cancel: Arc<AtomicBool>,
) -> Result<Outcome, Box<dyn Error>> {
    let options = CopyOptions {
        retries: args.retries,
        chunk_size: source::parse_size(&args.chunk_size)? as usize,
        zero_fill: !args.no_zero_fill,
        set_timestamps: !args.no_timestamps,
        on_existing: args.on_existing.into(),
        create_empty_dirs: !args.no_empty_dirs,
        ..CopyOptions::default()
    };

    let copier = Copier::new(source, &args.dest)
        .with_options(options)
        .with_cancel(Arc::clone(&cancel));

    if args.dry_run {
        let items = copier.plan()?;
        return Ok(print_plan(&items));
    }

    let tty = std::io::stderr().is_terminal();
    let report = copier.with_progress(progress_reporter(tty)).run()?;
    if tty {
        eprintln!();
    }

    print_summary(&report);
    write_reports(&report, args)?;

    Ok(if report.summary.is_complete() {
        Outcome::Complete
    } else {
        Outcome::Incomplete
    })
}

fn print_plan(items: &[ofr_copy::CopyItem]) -> Outcome {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for item in items {
        if item.is_dir() {
            println!("  [D] {}", item.path);
        } else {
            files += 1;
            bytes += item.size;
            println!(
                "  {:>10}  {}{}",
                format::bytes(item.size),
                item.path,
                if item.status == EntryStatus::Intact {
                    String::new()
                } else {
                    format!("  ({})", item.status.label())
                }
            );
        }
    }
    println!();
    println!(
        "対象: {files} 件 ({})。--dry-run なので何も書き出していない。",
        format::bytes(bytes)
    );
    if files == 0 {
        return Outcome::Incomplete;
    }
    Outcome::Complete
}

/// 進捗表示。端末なら 1 行を書き換え、そうでなければ 1 行ずつ追記する。
fn progress_reporter(tty: bool) -> impl FnMut(&CopyProgress) + Send + 'static {
    let mut last_width = 0usize;
    move |p| {
        let line = format!(
            "コピー {}/{} 件 ({:5.1}%)  {} / {}  速度 {}  残り {}  {}",
            p.files_done,
            p.files_total,
            p.ratio() * 100.0,
            format::bytes(p.bytes_done),
            format::bytes(p.bytes_total),
            format::rate(p.rate),
            format::eta(p.eta),
            p.current,
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

fn print_summary(report: &CopyReport) {
    let s = &report.summary;
    println!("---");
    println!(
        "コピー: {} 件 / {} (フォルダ {} 件)",
        s.copied,
        format::bytes(s.bytes_written),
        s.dirs
    );
    if s.partial > 0 {
        println!(
            "一部欠け: {} 件(読めずにゼロで埋めた分 {})",
            s.partial,
            format::bytes(s.bytes_missing)
        );
    }
    if s.failed > 0 {
        println!("失敗: {} 件", s.failed);
    }
    if s.skipped > 0 {
        println!("飛ばした: {} 件(宛先に同名のものがあった)", s.skipped);
    }
    println!("所要:   {}", format::duration(s.elapsed));

    for file in report.incomplete_files().take(10) {
        println!(
            "  ※ {} : {}{}",
            file.source,
            file.status,
            match &file.error {
                Some(e) => format!(" ({e})"),
                None => format!(" ({} バイトを埋めた)", file.missing),
            }
        );
    }
    let rest = report.incomplete_files().count().saturating_sub(10);
    if rest > 0 {
        println!("  ※ ほか {rest} 件(レポート参照)");
    }

    if s.cancelled {
        println!();
        println!("中断した。--on-existing skip を付けて同じコマンドを実行すると続きから進む。");
    }
    if s.partial > 0 || s.failed > 0 {
        println!();
        println!(
            "欠けたファイルが開けない場合、Phase 5 の修復モジュールで直せることがある(未実装)。"
        );
    }
}

fn write_reports(report: &CopyReport, args: &CopyArgs) -> Result<(), Box<dyn Error>> {
    if args.no_report {
        return Ok(());
    }
    let (json, text) = match &args.report {
        Some(path) => (path.clone(), path.with_extension("txt")),
        None => (args.dest.join(REPORT_JSON), args.dest.join(REPORT_TEXT)),
    };
    if let Some(dir) = json.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    report.write_json(&json)?;
    report.write_text(&text)?;
    println!("レポート: {} / {}", json.display(), text.display());
    Ok(())
}
