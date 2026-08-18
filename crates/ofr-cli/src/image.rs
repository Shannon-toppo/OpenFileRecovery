//! `ofr image`: デバイスを raw イメージへ吸い出す。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ofr_device::{Device, DeviceInfo, FileDevice};
use ofr_image::{ImageOptions, ImageSummary, Imager};

use crate::format;

/// `ofr image` の引数。
#[derive(Debug, clap::Args)]
pub struct ImageArgs {
    /// 復旧元。デバイス ID(`/dev/disk4`, `\\.\PhysicalDrive2`)か、既存のイメージファイル。
    pub source: String,

    /// 出力する raw イメージのパス。
    pub output: PathBuf,

    /// mapfile のパス。既定は `<出力>.map`。
    #[arg(short = 'm', long)]
    pub mapfile: Option<PathBuf>,

    /// mapfile を使わない(中断すると再開できなくなる)。
    #[arg(long, conflicts_with = "mapfile")]
    pub no_mapfile: bool,

    /// 不良セクタのリトライ回数。
    #[arg(short = 'r', long, default_value_t = 3)]
    pub retries: u32,

    /// コピーパスの読み込み単位(`1M`, `512K` のような接尾辞可)。
    #[arg(short = 'b', long, default_value = "1M")]
    pub block_size: String,

    /// トリム/スクレイプの粒度。既定はデバイスのセクタサイズ。
    #[arg(long)]
    pub sector_size: Option<String>,

    /// トリムパスを行わない。
    #[arg(long)]
    pub no_trim: bool,

    /// スクレイプパスを行わない。
    #[arg(long)]
    pub no_scrape: bool,

    /// リトライパスを行わない。
    #[arg(long)]
    pub no_retry: bool,

    /// 開始前にデバイスをアンマウントする(macOS の `diskutil unmountDisk`)。
    #[arg(long)]
    pub unmount: bool,

    /// mapfile のない既存イメージへの上書きを許可する。
    #[arg(long)]
    pub force: bool,
}

/// 実行結果。
pub enum Outcome {
    /// 全域を取得できた。
    Complete,
    /// 読めない領域が残った、または中断された。
    Incomplete,
}

/// イメージングを実行する。
pub fn run(args: ImageArgs) -> Result<Outcome, Box<dyn std::error::Error>> {
    // 起動ディスクの判定は列挙情報だけでできる。デバイスを開く前に弾く
    // (開くには管理者権限が要るので、権限エラーで理由が隠れないように)。
    check_source_selectable(&args.source)?;

    let device = open_source(&args.source)?;
    let info = device.info().clone();
    check_safety(&info, &args)?;

    if args.unmount {
        println!("{} をアンマウントする...", info.id);
        ofr_device::unmount_device(&info.id)?;
    }

    let map_path = if args.no_mapfile {
        None
    } else {
        Some(args.mapfile.clone().unwrap_or_else(|| {
            let mut p = args.output.clone().into_os_string();
            p.push(".map");
            PathBuf::from(p)
        }))
    };
    if args.output.exists() && !args.force {
        let resumable = map_path.as_deref().is_some_and(Path::exists);
        if !resumable {
            return Err(format!(
                "{} は既に存在する。再開用の mapfile もないので、上書きするなら --force を付ける",
                args.output.display()
            )
            .into());
        }
    }

    let tty = std::io::stderr().is_terminal();
    let options = build_options(&args, tty)?;

    println!(
        "復旧元: {} ({}, {})",
        info.id,
        info.display_name,
        format::bytes(device.len())
    );
    println!("出力:   {}", args.output.display());
    if let Some(p) = &map_path {
        println!("mapfile: {}", p.display());
    }
    println!();

    let cancel = Arc::new(AtomicBool::new(false));
    install_cancel_handler(Arc::clone(&cancel));

    let summary = Imager::new(device.as_ref())
        .with_options(options)
        .with_cancel(cancel)
        .with_progress(progress_reporter(tty))
        .run(&args.output, map_path.as_deref())?;

    if tty {
        eprintln!();
    }
    print_summary(&summary, map_path.as_deref());

    Ok(if summary.is_complete() {
        Outcome::Complete
    } else {
        Outcome::Incomplete
    })
}

/// 復旧元を開く。既存のファイルならイメージとして、それ以外はデバイス ID として扱う。
fn open_source(source: &str) -> Result<Box<dyn Device>, Box<dyn std::error::Error>> {
    let path = Path::new(source);
    if path.is_file() {
        Ok(Box::new(FileDevice::open(path)?))
    } else {
        Ok(ofr_device::open_device(source)?)
    }
}

/// 復旧元として選んでよいデバイスか、列挙情報だけで確かめる(PLAN.md 6章 3項)。
fn check_source_selectable(source: &str) -> Result<(), Box<dyn std::error::Error>> {
    if Path::new(source).is_file() {
        return Ok(()); // イメージファイルは対象外。
    }
    let Ok(devices) = ofr_device::list_devices() else {
        return Ok(()); // 列挙できない環境では、開いたあとの判定に任せる。
    };
    let Some(info) = devices.iter().find(|d| same_device(&d.id, source)) else {
        return Ok(());
    };
    if info.is_system_disk {
        return Err(format!("{} は起動ディスクなので復旧元にできない", info.id).into());
    }
    Ok(())
}

/// 安全原則(PLAN.md 6章)のチェック。
fn check_safety(info: &DeviceInfo, args: &ImageArgs) -> Result<(), Box<dyn std::error::Error>> {
    // 6章 3項: OS 起動ディスクは復旧元にできない。
    if info.is_system_disk {
        return Err(format!("{} は起動ディスクなので復旧元にできない", info.id).into());
    }

    // 6章 2項: 出力先が復旧元と同じデバイス上にあってはいけない。
    let dest_dir = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if let Some(dest_disk) = ofr_device::disk_id_for_path(&dest_dir)
        && same_device(&dest_disk, &info.id)
    {
        return Err(format!(
            "出力先 {} は復旧元 {} と同じデバイス上にある。別のディスクを指定すること",
            args.output.display(),
            info.id
        )
        .into());
    }
    Ok(())
}

/// デバイス ID の同一判定。`/dev/disk4` と `disk4` のような表記揺れを吸収する。
fn same_device(a: &str, b: &str) -> bool {
    fn key(s: &str) -> String {
        s.trim_start_matches(r"\\.\")
            .rsplit('/')
            .next()
            .unwrap_or(s)
            .trim_start_matches('r')
            .to_ascii_lowercase()
    }
    key(a) == key(b)
}

fn build_options(args: &ImageArgs, tty: bool) -> Result<ImageOptions, Box<dyn std::error::Error>> {
    let sector_size = match &args.sector_size {
        Some(s) => Some(u32::try_from(parse_size(s)?)?),
        None => None,
    };
    Ok(ImageOptions {
        chunk_size: parse_size(&args.block_size)?,
        sector_size,
        retries: args.retries,
        trim: !args.no_trim,
        scrape: !args.no_scrape,
        retry: !args.no_retry,
        // 端末でなければ 1 行ずつ流れるので、頻度を落とす。
        progress_interval: if tty {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        },
        ..ImageOptions::default()
    })
}

/// `4096` / `64K` / `1M` / `2G` を受け付ける。
fn parse_size(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let (digits, mult) = match t.chars().last() {
        Some('K') | Some('k') => (&t[..t.len() - 1], 1024),
        Some('M') | Some('m') => (&t[..t.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    let n: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("サイズとして読めない: {text}"))?;
    n.checked_mul(mult)
        .filter(|v| *v > 0)
        .ok_or_else(|| format!("サイズが範囲外: {text}"))
}

fn install_cancel_handler(cancel: Arc<AtomicBool>) {
    let result = ctrlc::set_handler(move || {
        eprintln!("\n中断する。mapfile を書き出すので、同じコマンドで再開できる。");
        cancel.store(true, Ordering::Relaxed);
    });
    if let Err(e) = result {
        tracing::warn!(error = %e, "Ctrl-C ハンドラを登録できなかった");
    }
}

/// 進捗表示。端末なら 1 行を書き換え、そうでなければ 1 行ずつ追記する。
fn progress_reporter(tty: bool) -> impl FnMut(&ofr_image::Progress) + Send + 'static {
    let mut last_width = 0usize;
    move |p| {
        let percent = if p.total > 0 {
            p.rescued as f64 / p.total as f64 * 100.0
        } else {
            0.0
        };
        let line = format!(
            "{} 取得 {} / {} ({percent:5.1}%)  速度 {}  残り {}  不良 {}  エラー {}",
            format::pad(p.pass.label(), 12),
            format::bytes(p.rescued),
            format::bytes(p.total),
            format::rate(p.rate),
            format::eta(p.eta),
            format::bytes(p.bad),
            p.errors,
        );
        let mut err = std::io::stderr();
        if tty {
            // 前の行が長かった場合に消し残しが出ないよう空白で埋める。
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

fn print_summary(summary: &ImageSummary, map_path: Option<&Path>) {
    println!("---");
    println!(
        "取得:   {} / {} ({:.2}%)",
        format::bytes(summary.rescued),
        format::bytes(summary.total),
        summary.rescued_ratio() * 100.0
    );
    println!("不良:   {}", format::bytes(summary.bad));
    println!("未取得: {}", format::bytes(summary.remaining));
    println!("エラー: {} 回", summary.errors);
    if summary.reopens > 0 {
        println!("開き直し: {} 回", summary.reopens);
    }
    println!("所要:   {}", format::duration(summary.elapsed));

    if summary.cancelled {
        println!();
        println!("中断した。同じコマンドを実行すれば mapfile から再開する。");
    } else if !summary.is_complete() {
        println!();
        println!("読めない領域が残っている。時間をおいて同じコマンドを実行すると、");
        println!("mapfile の不良領域だけを再試行する(--retries で回数を増やせる)。");
        if let Some(p) = map_path {
            println!("不良領域の一覧: {}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_with_suffixes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("64K").unwrap(), 64 * 1024);
        assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
        assert!(parse_size("").is_err());
        assert!(parse_size("0").is_err());
        assert!(parse_size("1.5M").is_err());
        assert!(parse_size("M").is_err());
    }

    #[test]
    fn compares_device_ids_across_spellings() {
        assert!(same_device("/dev/disk4", "/dev/rdisk4"));
        assert!(same_device("/dev/disk4", "disk4"));
        assert!(same_device(r"\\.\PhysicalDrive2", "PhysicalDrive2"));
        assert!(!same_device("/dev/disk4", "/dev/disk5"));
    }
}
