//! Open File Recovery の CLI。
//!
//! GUI より先に全機能をここで動かして検証する(PLAN.md 8章)。
//! Phase 4 の時点で使えるのは列挙・イメージング・解析・復元・カービング・コピーの 6 つ。
//!
//! ```text
//! ofr list
//! sudo ofr image /dev/disk4 /Volumes/Backup/usb.img
//! ofr scan /Volumes/Backup/usb.img
//! ofr restore /Volumes/Backup/usb.img /Volumes/Backup/recovered
//! ofr carve /Volumes/Backup/usb.img /Volumes/Backup/carved
//! ofr copy /Volumes/USB /Volumes/Backup/mirror
//! ```
//!
//! 生デバイスの読み込みには管理者 / root 権限が必要。

#![deny(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod carve;
mod copy;
mod filter;
mod format;
mod image;
mod list;
mod restore;
mod scan;
mod source;

/// 終了コード: 全域を取得できた。
const EXIT_OK: u8 = 0;
/// 終了コード: 読めない領域が残った、または中断した。
const EXIT_INCOMPLETE: u8 = 1;
/// 終了コード: 続行できないエラー。
const EXIT_ERROR: u8 = 2;

/// 何も見つからなかったときに出す案内。
const EXIT_HINT_EMPTY: &str = "条件に合う項目が見つからない。\n    フォーマット済みのデバイスなら孤立クラスタ走査 (既定で有効) が要る。\n    ファイルシステム自体が壊れている場合は `ofr carve` を試す (ファイル名は戻らない)。";

/// コマンドの実行結果。終了コードに対応する。
pub enum Outcome {
    /// やりたいことが全部できた。
    Complete,
    /// 一部しかできなかった、または中断した。
    Incomplete,
}

#[derive(Debug, Parser)]
#[command(
    name = "ofr",
    version,
    about = "Open File Recovery: USB メモリ / SD カードのデータ復旧",
    long_about = "USB メモリと SD カードのデータ復旧ツール。\n\
                  復旧元デバイスへは一切書き込まない。\n\
                  壊れかけたメディアは、まずイメージを取ってからイメージを解析すること。"
)]
struct Cli {
    /// ログを詳しく出す(-v で debug、-vv で trace)。
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 接続されているデバイスを一覧する。
    List {
        /// JSON で出力する。
        #[arg(long)]
        json: bool,
    },

    /// デバイスを raw イメージへ吸い出す(壊れかけメディア対応)。
    Image(image::ImageArgs),

    /// ファイルシステムを解析して、復元できる項目を一覧する。
    Scan(scan::ScanArgs),

    /// 見つかった項目を復元先フォルダへ書き出す。
    Restore(restore::RestoreArgs),

    /// ファイルシステムに頼らず、シグネチャからファイルを探して切り出す。
    Carve(carve::CarveArgs),

    /// デバイスの中身をフォルダ構造ごと宛先へ写す。
    Copy(copy::CopyArgs),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let result = match cli.command {
        Command::List { json } => list::run(json)
            .map(|()| Outcome::Complete)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) }),
        Command::Image(args) => image::run(args),
        Command::Scan(args) => scan::run(args),
        Command::Restore(args) => restore::run(args),
        Command::Carve(args) => carve::run(args),
        Command::Copy(args) => copy::run(args),
    };

    match result {
        Ok(Outcome::Complete) => ExitCode::from(EXIT_OK),
        Ok(Outcome::Incomplete) => ExitCode::from(EXIT_INCOMPLETE),
        Err(e) => {
            eprintln!("エラー: {e}");
            let mut source = e.source();
            while let Some(s) = source {
                eprintln!("  原因: {s}");
                source = s.source();
            }
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn init_tracing(verbose: u8) {
    // クレート名は ofr_device / ofr_image / ofr_fs / ofr_fat / ofr_exfat / ofr_carve /
    // ofr_copy / ofr_cli になる。
    let level = match verbose {
        0 => {
            "warn,ofr_cli=info,ofr_image=info,ofr_device=info,ofr_fs=info,ofr_fat=info,ofr_exfat=info,ofr_carve=info,ofr_copy=info"
        }
        1 => {
            "info,ofr_cli=debug,ofr_image=debug,ofr_device=debug,ofr_fs=debug,ofr_fat=debug,ofr_exfat=debug,ofr_carve=debug,ofr_copy=debug"
        }
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("OFR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
