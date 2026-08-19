//! 破損ファイルの修復。復元したファイルが開けないときの最後の手当て
//! (PLAN.md 5.6)。
//!
//! 対象は JPEG / PNG / AVI / MP4(MOV を含む)の 4 形式。どれも
//! 「コンテナ構造の再建と、デコードできる範囲のサルベージ」が守備範囲で、
//! 失われた画素や失われたフレームそのものを作り出すことはしない。
//!
//! ```no_run
//! use std::path::Path;
//! use ofr_repair::{RepairStatus, Repairer};
//!
//! let report = Repairer::new("recovered/IMG_0042.jpg", "repaired/IMG_0042.jpg")
//!     // 同じカメラで撮った正常なファイルがあると精度が上がる。
//!     .with_reference("reference/IMG_0001.jpg")
//!     .run()?;
//!
//! println!("{}", report.text_summary());
//! assert_ne!(report.status, RepairStatus::Failed);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # 形式ごとにできること
//!
//! | 形式 | 直せるもの | 直せないもの |
//! |---|---|---|
//! | JPEG | ヘッダ(SOI〜SOS)の破損、途中切断、末尾のごみ | 失われたエントロピー符号そのもの |
//! | PNG | CRC 不一致、チャンク長の破損、IEND 欠損、IDAT の部分破損 | 壊れた画素の中身 |
//! | AVI | idx1 の欠損・破損、RIFF / LIST サイズの破損、avih / strh の値 | 失われたフレーム |
//! | MP4 | moov の欠損(参照ファイル方式)、mdat サイズの破損、末尾切断 | 参照なしでの完全な moov 再構築 |
//!
//! # 参照ファイル
//!
//! JPEG と MP4 は「同じ機器・同じ設定で作られた正常なファイル」を渡せると精度が
//! 大きく上がる。JPEG はヘッダ(量子化表・ハフマン表・寸法)を、MP4 は moov の
//! 構造とコーデック設定を雛形として借りる。AVI も hdrl が丸ごと消えている場合は
//! 参照から借りる。
//!
//! MP4 の moov 再構築は参照なしだと成功率が大きく落ちる。これは本ソフトの
//! 実装が弱いのではなく、市販ソフトを含めて原理的にそうなる(PLAN.md 10章)。
//!
//! # 安全原則
//!
//! - **修復は必ずコピーに対して行い、元ファイルは残す**(PLAN.md 5.6)。入力は
//!   [`ofr_device::Device`] 経由で開いていて、書き込み経路がそもそも無い。
//!   出力先が入力と同じなら [`RepairError::SameFile`] で始める前に止める。
//! - ffmpeg には依存しない。同梱もしないし、入っているものを探して呼ぶこともしない
//!   (挙動がバージョン依存になってテスト不能になるため)。
//! - 壊れたバイト列で panic しない(PLAN.md 6章 5項)。全てのオフセットは
//!   範囲確認してから使う。

#![deny(unsafe_code)]

mod avi;
mod error;
mod format;
mod jpeg;
mod mp4;
mod options;
mod png;
mod report;
mod source;
mod verify;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub use error::{RepairError, Result};
pub use format::RepairFormat;
pub use options::{DEFAULT_FILL, DEFAULT_MAX_IN_MEMORY, RepairOptions};
pub use report::{RepairReport, RepairStatus, Verification};

use source::Source;

/// 修復を 1 回行う。PLAN.md 5.6 の共通 API。
///
/// 出力先は入力と別のパスでなければならない。
pub fn repair(input: &Path, reference: Option<&Path>, output: &Path) -> Result<RepairReport> {
    let mut r = Repairer::new(input, output);
    if let Some(reference) = reference {
        r = r.with_reference(reference);
    }
    r.run()
}

/// 修復の実行単位。
///
/// [`repair`] は薄い包みで、設定を変えたいときはこちらを使う。
#[derive(Debug, Clone)]
pub struct Repairer {
    input: PathBuf,
    output: PathBuf,
    reference: Option<PathBuf>,
    options: RepairOptions,
}

impl Repairer {
    /// 修復元と出力先を指定して作る。
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            input: input.into(),
            output: output.into(),
            reference: None,
            options: RepairOptions::default(),
        }
    }

    /// 参照ファイル(同じ機器・同じ設定で作られた正常なファイル)を指定する。
    pub fn with_reference(mut self, reference: impl Into<PathBuf>) -> Self {
        self.reference = Some(reference.into());
        self
    }

    /// 設定を差し替える。
    pub fn with_options(mut self, options: RepairOptions) -> Self {
        self.options = options;
        self
    }

    /// 修復する。
    pub fn run(self) -> Result<RepairReport> {
        let started = Instant::now();
        check_not_same_file(&self.input, &self.output)?;

        let mut src = Source::open(&self.input)?;
        let format = match self.options.format {
            Some(f) => f,
            None => {
                format::detect(&mut src, &self.input).ok_or_else(|| RepairError::UnknownFormat {
                    path: self.input.clone(),
                })?
            }
        };

        let mut report = RepairReport::new(&self.input, format, src.len());
        report.reference = self.reference.clone();
        if let Some(parent) = self.output.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| RepairError::output(parent, e))?;
        }

        let mut job = Job {
            src: &mut src,
            reference: self.reference.as_deref(),
            options: &self.options,
            output: &self.output,
            report: &mut report,
        };

        match format {
            RepairFormat::Jpeg => jpeg::repair(&mut job)?,
            RepairFormat::Png => png::repair(&mut job)?,
            RepairFormat::Avi => avi::repair(&mut job)?,
            RepairFormat::Mp4 => mp4::repair(&mut job)?,
        }

        // 検証が落ちたものを「修復した」と言い切らない。復旧ソフトのレポートで
        // 一番やってはいけないのが、直っていないものを直ったことにすること。
        if report.status == RepairStatus::Repaired
            && matches!(report.verification, Verification::Failed(_))
        {
            report.status = RepairStatus::Partial;
        }
        report.elapsed = started.elapsed();
        Ok(report)
    }
}

/// 入力と出力が同じファイルを指していないか確かめる(PLAN.md 5.6)。
fn check_not_same_file(input: &Path, output: &Path) -> Result<()> {
    if input == output {
        return Err(RepairError::SameFile {
            path: input.to_path_buf(),
        });
    }
    // シンボリックリンクや `./` 表記で同じ実体を指している場合を拾う。
    // 出力がまだ無い場合は canonicalize が失敗するので、そのときは上の比較で足りる。
    if let (Ok(a), Ok(b)) = (input.canonicalize(), output.canonicalize())
        && a == b
    {
        return Err(RepairError::SameFile {
            path: input.to_path_buf(),
        });
    }
    Ok(())
}

/// 修復モジュールに渡す一式。
pub(crate) struct Job<'a> {
    /// 修復元。読み込みしかできない。
    pub src: &'a mut Source,
    /// 参照ファイル。
    pub reference: Option<&'a Path>,
    /// 設定。
    pub options: &'a RepairOptions,
    /// 出力先。
    pub output: &'a Path,
    /// 埋めていくレポート。
    pub report: &'a mut RepairReport,
}

impl Job<'_> {
    /// 組み立てた静止画を書き出し、検証してレポートを締める。
    ///
    /// `status` は形式側の判断(何をどこまで直したか)で、検証に落ちた場合は
    /// [`Repairer::run`] が `Partial` に落とす。
    pub(crate) fn finish_image(&mut self, data: &[u8], status: RepairStatus) -> Result<()> {
        if status == RepairStatus::Intact && !self.options.write_intact {
            self.report.status = status;
            self.report.verification =
                Verification::Skipped("元から壊れていないので書き出していない".to_string());
            return Ok(());
        }

        if self.options.verify {
            let check = verify::image_check(self.report.format, data, self.options.fill);
            if let Some(note) = check.note {
                self.report.issue(note);
            }
            self.report.verification = check.verification;
        } else {
            self.report.verification = Verification::Skipped("検証を無効にしている".to_string());
        }

        fs::write(self.output, data).map_err(|e| RepairError::output(self.output, e))?;
        self.report.output = Some(self.output.to_path_buf());
        self.report.output_size = data.len() as u64;
        self.report.status = status;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_to_overwrite_the_input() {
        let err = Repairer::new("a.jpg", "a.jpg").run().unwrap_err();
        assert!(matches!(err, RepairError::SameFile { .. }));
    }

    #[test]
    fn reports_unknown_formats() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("mystery.bin");
        fs::write(&input, b"nothing recognisable in here at all").unwrap();
        let err = Repairer::new(&input, dir.path().join("out.bin"))
            .run()
            .unwrap_err();
        assert!(matches!(err, RepairError::UnknownFormat { .. }));
    }
}
