//! `ofr image`: デバイスを raw イメージへ吸い出す。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use ofr_core::replug::ReplugState;
use ofr_device::{Device, DeviceInfo};
use ofr_image::{ImageOptions, ImageSummary, Imager, StallAction};

use crate::Outcome;
use crate::format;
use crate::source::{self, install_cancel_handler, parse_duration, parse_size, same_device};

/// 応答しない読み込みを見捨てる上限。これを超えたら打ち切って抜き差しを促す。
const MAX_ABANDONED_READS: u32 = ofr_device::DEFAULT_MAX_ABANDONED;

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

    /// 1 回の読み込みを見限るまでの時間(`30s`, `500ms`)。`0` で制限なし。
    ///
    /// 固まったメディアは読み込みが返ってこなくなる。制限時間を過ぎたら
    /// その読み込みを見捨てて先へ進む(PLAN.md 11.2)。
    #[arg(long, default_value = "30s")]
    pub io_timeout: String,

    /// 読み込みに失敗したときに読み飛ばす量の上限。
    #[arg(long, default_value = "16M")]
    pub skip_size: String,

    /// 失敗しても読み飛ばさない(従来どおり順に読む)。
    #[arg(long, conflicts_with = "skip_size")]
    pub no_skip: bool,

    /// スクレイプ/リトライの探索単位の上限。セクタサイズにすると総当たりになる。
    #[arg(long, default_value = "64K")]
    pub probe_size: String,

    /// 読めなかった領域だけを再試行する(未試行の領域には触らない)。
    ///
    /// 時間をおいて、冷ましてから、別のリーダーで、といった条件を変えた
    /// 再挑戦に使う。前回の mapfile が要る。
    #[arg(long)]
    pub retry_only: bool,

    /// 前回「読めなかった」と記録した領域を、未試行に戻してから始める。
    #[arg(long, conflicts_with = "retry_only")]
    pub reset_bad: bool,

    /// 読む範囲を、この mapfile の取得済み (`+`) 領域だけに限る。
    ///
    /// `ofr scan --domain-out` で書き出したものを渡すと、選んだファイルの
    /// クラスタ範囲だけを取り直せる。GNU ddrescue の `--domain-mapfile` と同じ形式。
    #[arg(long, value_name = "MAPFILE")]
    pub domain: Option<PathBuf>,

    /// ブートセクタや FAT 表を先に確保せず、頭から順に読む。
    #[arg(long)]
    pub no_metadata_first: bool,

    /// 末尾から手前へ向かって読む。
    ///
    /// ブリッジの先読みが不良セクタの手前まで巻き添えにする個体では、
    /// 反対側から詰めると 1 個ぶん多く取れることがある(PLAN.md 11.5)。
    #[arg(long)]
    pub reverse: bool,

    /// 読めなかったセクタを別経路で読み直さない。
    ///
    /// 既定では、リトライパスで通常経路が駄目だったセクタを別経路
    /// (macOS はバッファ付きのブロックデバイス、Windows はバッファ付きハンドル)
    /// でもう一度読む。
    #[arg(long)]
    pub no_alt_route: bool,

    /// リトライで拾えた範囲を N 回読んで多数決を取る(`0`/`1` で無効)。
    ///
    /// 読むたびに内容が変わる個体で効く。揺れた範囲は「不確か」として mapfile に
    /// 記録する(PLAN.md 11.5)。
    #[arg(long, default_value_t = 0, value_name = "N")]
    pub verify: u32,

    /// デバイスが応答しなくなっても、抜き差しを待たずに打ち切る。
    #[arg(long)]
    pub no_wait: bool,

    /// 抜き差しを待つ時間の上限(`5m`, `30s`)。
    #[arg(long, default_value = "5m", conflicts_with = "no_wait")]
    pub wait_timeout: String,

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

/// イメージングを実行する。
pub fn run(args: ImageArgs) -> Result<Outcome, Box<dyn std::error::Error>> {
    // 起動ディスクの判定は列挙情報だけでできる。デバイスを開く前に弾く
    // (開くには管理者権限が要るので、権限エラーで理由が隠れないように)。
    source::check_source_selectable(&args.source)?;

    let io_timeout = parse_duration(&args.io_timeout)?;
    let device = source::open_source_with_timeout(&args.source, io_timeout, MAX_ABANDONED_READS)?;
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
    let mut options = build_options(&args, tty)?;

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
    if !io_timeout.is_zero() && !std::path::Path::new(&args.source).is_file() {
        println!("読み込みの制限時間: {}", format::duration(io_timeout));
    }

    // 読む範囲の限定と、先に確保する範囲(PLAN.md 11.4)。
    if let Some(path) = &args.domain {
        let domain = ofr_image::Domain::load(path)?;
        if domain.is_empty() {
            return Err(format!("{} に取得対象の領域がない", path.display()).into());
        }
        println!(
            "対象範囲: {} ({} 区間, {} から)",
            format::bytes(domain.total()),
            domain.regions().len(),
            path.display(),
        );
        options.domain = Some(domain);
    }
    if !args.no_metadata_first && !args.retry_only {
        options.priority = metadata_priority(device.as_ref());
    }
    println!();

    let cancel = Arc::new(AtomicBool::new(false));
    install_cancel_handler(
        Arc::clone(&cancel),
        "中断する。mapfile を書き出すので、同じコマンドで再開できる。",
    );

    let mut imager = Imager::new(device.as_ref())
        .with_options(options)
        .with_cancel(Arc::clone(&cancel))
        .with_progress(progress_reporter(tty));
    if !args.no_wait {
        imager = imager.with_stall_handler(stall_handler(
            info.clone(),
            parse_duration(&args.wait_timeout)?,
            Arc::clone(&cancel),
            tty,
        ));
    }
    let summary = imager.run(&args.output, map_path.as_deref())?;

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

/// メタデータの位置を割り出して、先に確保する範囲にする(PLAN.md 11.4)。
///
/// ブートセクタ、FAT / アロケーションビットマップ、ルートディレクトリを最初に
/// 取っておけば、途中で読めなくなっても「どこに何があったか」は残る。
/// 位置が分からなければ何もしない(頭から順に読む)。
fn metadata_priority(device: &dyn Device) -> Option<ofr_image::Domain> {
    let plan = ofr_core::domain::metadata_plan(device);
    if plan.is_empty() {
        println!("メタデータの位置が分からないので、先頭から順に読む。");
        return None;
    }

    // 同じラベルが並ぶ(FAT 表が 2 枚など)ので、続きは 1 つにまとめて出す。
    let mut labels: Vec<&str> = Vec::new();
    for region in &plan.regions {
        if labels.last() != Some(&region.label.as_str()) {
            labels.push(&region.label);
        }
    }
    println!(
        "先に取る: {} ({})",
        labels.join("、"),
        format::bytes(plan.domain.total()),
    );
    Some(plan.domain)
}

fn build_options(args: &ImageArgs, tty: bool) -> Result<ImageOptions, Box<dyn std::error::Error>> {
    let sector_size = match &args.sector_size {
        Some(s) => Some(u32::try_from(parse_size(s)?)?),
        None => None,
    };
    Ok(ImageOptions {
        chunk_size: parse_size(&args.block_size)?,
        skip_size_max: if args.no_skip {
            0
        } else {
            parse_size(&args.skip_size)?
        },
        probe_size_max: parse_size(&args.probe_size)?,
        sector_size,
        retries: args.retries,
        trim: !args.no_trim,
        scrape: !args.no_scrape,
        retry: !args.no_retry,
        retry_only: args.retry_only,
        reset_bad: args.reset_bad,
        reverse: args.reverse,
        alt_route: !args.no_alt_route,
        verify: args.verify,
        // 端末でなければ 1 行ずつ流れるので、頻度を落とす。
        progress_interval: if tty {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        },
        ..ImageOptions::default()
    })
}

/// デバイスが応答しなくなったときの受け答え(PLAN.md 11.3)。
///
/// 固まったコントローラは電源を入れ直す以外に戻らないので、抜き差しを頼んで待つ。
/// 抜かれたことを確かめてから、挿し直されるのを待つ(応答しないだけで OS からは
/// 見えたままのことが多く、それを「戻った」と誤解すると同じ所で固まるため)。
fn stall_handler(
    info: DeviceInfo,
    wait_timeout: Duration,
    cancel: Arc<AtomicBool>,
    tty: bool,
) -> impl FnMut(&ofr_image::Stall) -> StallAction + Send + 'static {
    move |stall| {
        let mut err = std::io::stderr();
        if tty {
            // 進捗行を書きかけのまま残さない。
            let _ = writeln!(err);
        }
        let _ = writeln!(
            err,
            "デバイスが応答しない({} 回目)。メディアをいったん抜いて、数秒おいてから挿し直すこと。",
            stall.attempt
        );
        let _ = writeln!(
            err,
            "  {} まで待つ。中断するなら Ctrl-C。",
            format::duration(wait_timeout)
        );
        let _ = err.flush();

        let back = ofr_core::replug::wait_for_replug(&info, wait_timeout, &cancel, |state| {
            let mut err = std::io::stderr();
            let line = match state {
                ReplugState::WaitingInsertion => {
                    "  デバイスが外れた。挿し直すのを待っている...".to_string()
                }
                ReplugState::Back(id) => format!("  {id} が戻った。続きから読み直す。"),
                ReplugState::TimedOut => "  待ったが戻ってこないので打ち切る。".to_string(),
                ReplugState::Cancelled => "  中断された。".to_string(),
                _ => return,
            };
            let _ = writeln!(err, "{line}");
            let _ = err.flush();
        });

        if back.is_some() {
            StallAction::Resume
        } else {
            StallAction::Abort
        }
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
    if summary.timeouts > 0 {
        println!(
            "応答なし: {} 回(制限時間で見捨てた読み込み)",
            summary.timeouts
        );
    }
    if summary.stalls > 0 {
        println!("抜き差し待ち: {} 回", summary.stalls);
    }
    if summary.alt_rescues > 0 {
        println!(
            "別経路で回収: {}(通常の経路では読めなかった分)",
            format::bytes(summary.alt_rescues)
        );
    }
    if summary.resized_rescues > 0 {
        println!(
            "転送単位を変えて回収: {}",
            format::bytes(summary.resized_rescues)
        );
    }
    if summary.uncertain > 0 {
        println!(
            "不確か: {}(読むたびに中身が変わった範囲)",
            format::bytes(summary.uncertain)
        );
    }
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
        if summary.timeouts > 0 {
            println!("応答が返らなくなった読み込みがある。メディアを抜き差ししてから");
            println!("再開すると、コントローラが復帰して読めることがある。");
        }
        println!(
            "読めなかった所だけを狙うなら --retry-only、記録を捨ててやり直すなら --reset-bad。"
        );
        println!("向きを変えて読み直す --reverse も、個体によっては効く。");
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
