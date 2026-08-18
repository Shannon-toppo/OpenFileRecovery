//! Open File Recovery の CLI。
//!
//! GUI より先に全機能をここで動かして検証する(PLAN.md 8章)。
//! Phase 1 の時点で使えるのはデバイス列挙とイメージングの 2 つ。
//!
//! ```text
//! ofr list
//! sudo ofr image /dev/disk4 /Volumes/Backup/usb.img
//! ```
//!
//! 生デバイスの読み込みには管理者 / root 権限が必要。

#![deny(unsafe_code)]

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod format;
mod image;
mod list;

/// 終了コード: 全域を取得できた。
const EXIT_OK: u8 = 0;
/// 終了コード: 読めない領域が残った、または中断した。
const EXIT_INCOMPLETE: u8 = 1;
/// 終了コード: 続行できないエラー。
const EXIT_ERROR: u8 = 2;

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    let result = match cli.command {
        Command::List { json } => list::run(json)
            .map(|()| image::Outcome::Complete)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) }),
        Command::Image(args) => image::run(args),
    };

    match result {
        Ok(image::Outcome::Complete) => ExitCode::from(EXIT_OK),
        Ok(image::Outcome::Incomplete) => ExitCode::from(EXIT_INCOMPLETE),
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
    // クレート名は ofr_device / ofr_image / ofr_cli になる。
    let level = match verbose {
        0 => "warn,ofr_cli=info,ofr_image=info,ofr_device=info",
        1 => "info,ofr_cli=debug,ofr_image=debug,ofr_device=debug",
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
