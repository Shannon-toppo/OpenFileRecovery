//! `ofr repair`: 開けなくなったファイルを直す。
//!
//! 復元やカービングで拾ったファイルが開けないときに使う(PLAN.md 5.6)。
//! 修復元は絶対に書き換えないので、出力先は必ず別のパスを指定する。
//!
//! ```text
//! ofr repair recovered/IMG_0042.jpg repaired/IMG_0042.jpg
//! ofr repair broken.mp4 fixed.mp4 --reference 同じカメラの正常な録画.mp4
//! ```

use std::path::PathBuf;

use ofr_repair::{RepairFormat, RepairOptions, RepairStatus, Repairer};

use crate::Outcome;

/// `--format` の選択肢。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FormatChoice {
    /// 中身から判定する(既定)。
    Auto,
    /// JPEG。
    Jpeg,
    /// PNG。
    Png,
    /// AVI。
    Avi,
    /// MP4 / MOV。
    Mp4,
}

impl From<FormatChoice> for Option<RepairFormat> {
    fn from(value: FormatChoice) -> Self {
        match value {
            FormatChoice::Auto => None,
            FormatChoice::Jpeg => Some(RepairFormat::Jpeg),
            FormatChoice::Png => Some(RepairFormat::Png),
            FormatChoice::Avi => Some(RepairFormat::Avi),
            FormatChoice::Mp4 => Some(RepairFormat::Mp4),
        }
    }
}

/// `ofr repair` の引数。
#[derive(Debug, clap::Args)]
pub struct RepairArgs {
    /// 直したいファイル。中身は書き換えない。
    pub input: PathBuf,

    /// 修復結果の出力先。修復元と同じパスは指定できない。
    pub output: PathBuf,

    /// 参照ファイル。同じ機器・同じ設定で作られた**正常な**ファイルを指定する。
    ///
    /// JPEG はヘッダ(量子化表・ハフマン表・寸法)を、MP4 は moov の構造と
    /// コーデック設定を、AVI は hdrl を、ここから借りる。
    #[arg(short, long)]
    pub reference: Option<PathBuf>,

    /// 形式を指定する(既定は中身から判定)。
    #[arg(long, value_enum, default_value = "auto")]
    pub format: FormatChoice,

    /// 画像の幅。ヘッダが失われていて参照ファイルも無い場合に使う。
    #[arg(long)]
    pub width: Option<u32>,

    /// 画像の高さ。
    #[arg(long)]
    pub height: Option<u32>,

    /// 欠けた画素を埋める値(0〜255)。既定は中間グレー。
    #[arg(long, default_value_t = ofr_repair::DEFAULT_FILL)]
    pub fill: u8,

    /// 修復結果の検証(静止画のデコード / 動画のコンテナ整合性)を行わない。
    #[arg(long)]
    pub no_verify: bool,

    /// 元から壊れていなかった場合は出力を書かない。
    #[arg(long)]
    pub skip_intact: bool,

    /// JSON レポートの出力先。
    #[arg(long)]
    pub report: Option<PathBuf>,
}

/// 修復を実行する。
pub fn run(args: RepairArgs) -> Result<Outcome, Box<dyn std::error::Error>> {
    let options = RepairOptions {
        format: args.format.into(),
        verify: !args.no_verify,
        fill: args.fill,
        write_intact: !args.skip_intact,
        width: args.width,
        height: args.height,
        ..RepairOptions::default()
    };

    let mut repairer = Repairer::new(&args.input, &args.output).with_options(options);
    if let Some(reference) = &args.reference {
        repairer = repairer.with_reference(reference);
    }

    let report = repairer.run()?;
    print!("{}", report.text_summary());

    if let Some(path) = &args.report {
        report.write_json(path)?;
        println!("\nJSON レポート: {}", path.display());
    }

    // 直しきれなかったものは、それと分かる終了コードで返す。
    Ok(match report.status {
        RepairStatus::Intact | RepairStatus::Repaired => Outcome::Complete,
        RepairStatus::Partial | RepairStatus::Failed => Outcome::Incomplete,
    })
}
