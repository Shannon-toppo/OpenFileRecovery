//! コピー結果のレポート。
//!
//! PLAN.md 5.5 の「完了時に結果レポート(全ファイルの成否とエラー内容の JSON +
//! 人間向けサマリ)を宛先に書き出す」がこれ。JSON は GUI と後処理が読むためのもので、
//! テキストは利用者がそのまま読むためのもの。
//!
//! JSON は手書きで組み立てる(この程度の構造に serde を足す理由がない)。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{CopyError, Result};

/// JSON レポートの既定のファイル名。
pub const REPORT_JSON: &str = "ofr-copy-report.json";
/// テキストレポートの既定のファイル名。
pub const REPORT_TEXT: &str = "ofr-copy-report.txt";

/// 1 ファイルのコピー結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStatus {
    /// 全部読めて、そのまま宛先に入った。
    Copied,
    /// 一部が読めなかった。読めた分は入っていて、読めなかった所はゼロで埋めてある。
    Partial,
    /// 1 バイトも救えなかった。
    Failed,
    /// 宛先に既にあったので飛ばした。
    Skipped,
}

impl CopyStatus {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            CopyStatus::Copied => "コピー済み",
            CopyStatus::Partial => "一部欠け",
            CopyStatus::Failed => "失敗",
            CopyStatus::Skipped => "飛ばした",
        }
    }

    /// JSON に出す機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            CopyStatus::Copied => "copied",
            CopyStatus::Partial => "partial",
            CopyStatus::Failed => "failed",
            CopyStatus::Skipped => "skipped",
        }
    }
}

impl std::fmt::Display for CopyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// レポートに載せる 1 ファイルの記録。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileResult {
    /// 復旧元でのパス。
    pub source: String,
    /// 書き出した先。
    pub output: PathBuf,
    /// 元のサイズ。
    pub size: u64,
    /// 実際に書き出したバイト数。
    pub written: u64,
    /// 読めずにゼロで埋めたバイト数。
    pub missing: u64,
    /// 読み込みエラーの回数。
    pub read_errors: u32,
    /// 結果。
    pub status: CopyStatus,
    /// 失敗した場合の理由。
    pub error: Option<String>,
}

impl FileResult {
    /// 欠けなくコピーできたか。
    pub fn is_complete(&self) -> bool {
        self.status == CopyStatus::Copied
    }
}

/// コピー全体のサマリ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CopySummary {
    /// 対象になったファイル数。
    pub files: u64,
    /// 欠けなくコピーできたファイル数。
    pub copied: u64,
    /// 一部が読めなかったファイル数。
    pub partial: u64,
    /// 失敗したファイル数。
    pub failed: u64,
    /// 飛ばしたファイル数。
    pub skipped: u64,
    /// 作ったディレクトリ数。
    pub dirs: u64,
    /// 対象の合計バイト数。
    pub bytes_expected: u64,
    /// 書き出した合計バイト数。
    pub bytes_written: u64,
    /// 読めずに埋めた合計バイト数。
    pub bytes_missing: u64,
    /// 所要時間。
    pub elapsed: Duration,
    /// 中断されたか。
    pub cancelled: bool,
}

impl CopySummary {
    /// 欠けも失敗も中断もなかったか。
    pub fn is_complete(&self) -> bool {
        !self.cancelled && self.failed == 0 && self.partial == 0
    }
}

/// コピーの結果一式。
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CopyReport {
    /// 復旧元の名前。
    pub source: String,
    /// 宛先。
    pub destination: PathBuf,
    /// サマリ。
    pub summary: CopySummary,
    /// ファイルごとの記録。処理した順。
    pub files: Vec<FileResult>,
    /// 気付いたこと(飛ばしたシンボリックリンク、開けなかったフォルダなど)。
    pub warnings: Vec<String>,
}

impl CopyReport {
    /// 一部でも欠けたファイル。
    pub fn incomplete_files(&self) -> impl Iterator<Item = &FileResult> {
        self.files.iter().filter(|f| !f.is_complete())
    }

    /// JSON レポートとテキストレポートを宛先フォルダへ書き出す。
    ///
    /// 戻り値は書いた 2 つのパス。
    pub fn write_to_dir(&self, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        let json = dir.join(REPORT_JSON);
        let text = dir.join(REPORT_TEXT);
        self.write_json(&json)?;
        self.write_text(&text)?;
        Ok((json, text))
    }

    /// JSON レポートを書き出す。
    pub fn write_json(&self, path: &Path) -> Result<()> {
        let file = File::create(path).map_err(|e| CopyError::output(path, e))?;
        let mut out = BufWriter::new(file);
        self.render_json(&mut out)
            .map_err(|e| CopyError::output(path, e))
    }

    /// 人間向けのテキストレポートを書き出す。
    pub fn write_text(&self, path: &Path) -> Result<()> {
        let file = File::create(path).map_err(|e| CopyError::output(path, e))?;
        let mut out = BufWriter::new(file);
        out.write_all(self.text_summary().as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| CopyError::output(path, e))
    }

    /// JSON を書き出す。
    pub fn render_json(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(out, "{{")?;
        writeln!(out, "  \"source\": {},", json_string(&self.source))?;
        writeln!(
            out,
            "  \"destination\": {},",
            json_string(&self.destination.display().to_string())
        )?;
        writeln!(out, "  \"files\": [")?;
        for (i, f) in self.files.iter().enumerate() {
            let comma = if i + 1 == self.files.len() { "" } else { "," };
            writeln!(
                out,
                concat!(
                    "    {{\"source\": {}, \"output\": {}, \"size\": {}, \"written\": {}, ",
                    "\"missing\": {}, \"read_errors\": {}, \"status\": {}, \"error\": {}}}{}"
                ),
                json_string(&f.source),
                json_string(&f.output.display().to_string()),
                f.size,
                f.written,
                f.missing,
                f.read_errors,
                json_string(f.status.as_str()),
                match &f.error {
                    Some(e) => json_string(e),
                    None => "null".to_string(),
                },
                comma
            )?;
        }
        writeln!(out, "  ],")?;

        writeln!(out, "  \"warnings\": [")?;
        for (i, w) in self.warnings.iter().enumerate() {
            let comma = if i + 1 == self.warnings.len() {
                ""
            } else {
                ","
            };
            writeln!(out, "    {}{comma}", json_string(w))?;
        }
        writeln!(out, "  ],")?;

        let s = &self.summary;
        writeln!(
            out,
            concat!(
                "  \"summary\": {{\"files\": {}, \"copied\": {}, \"partial\": {}, ",
                "\"failed\": {}, \"skipped\": {}, \"dirs\": {}, \"bytes_expected\": {}, ",
                "\"bytes_written\": {}, \"bytes_missing\": {}, \"elapsed_ms\": {}, ",
                "\"cancelled\": {}, \"complete\": {}}}"
            ),
            s.files,
            s.copied,
            s.partial,
            s.failed,
            s.skipped,
            s.dirs,
            s.bytes_expected,
            s.bytes_written,
            s.bytes_missing,
            s.elapsed.as_millis(),
            s.cancelled,
            s.is_complete(),
        )?;
        writeln!(out, "}}")?;
        out.flush()
    }

    /// 人間向けサマリ。テキストレポートの中身であり、CLI の画面出力でもある。
    pub fn text_summary(&self) -> String {
        let s = &self.summary;
        let mut out = String::new();
        out.push_str("Open File Recovery コピー結果\n");
        out.push_str("=============================\n\n");
        out.push_str(&format!("復旧元: {}\n", self.source));
        out.push_str(&format!("宛先:   {}\n", self.destination.display()));
        out.push_str(&format!("所要:   {} 秒\n\n", s.elapsed.as_secs()));

        out.push_str(&format!("ファイル: {} 件\n", s.files));
        out.push_str(&format!("  コピー済み: {} 件\n", s.copied));
        out.push_str(&format!("  一部欠け:   {} 件\n", s.partial));
        out.push_str(&format!("  失敗:       {} 件\n", s.failed));
        if s.skipped > 0 {
            out.push_str(&format!("  飛ばした:   {} 件\n", s.skipped));
        }
        out.push_str(&format!("フォルダ: {} 件\n", s.dirs));
        out.push_str(&format!("書き出し: {}\n", bytes(s.bytes_written)));
        if s.bytes_missing > 0 {
            out.push_str(&format!("読めずに埋めた分: {}\n", bytes(s.bytes_missing)));
        }
        if s.cancelled {
            out.push_str("\n※ 中断された。ここまでに書いたファイルは残っている。\n");
        }

        let broken: Vec<&FileResult> = self.incomplete_files().collect();
        if !broken.is_empty() {
            out.push_str("\n欠けたファイル\n--------------\n");
            for f in broken {
                match &f.error {
                    Some(e) => out.push_str(&format!("{} : {} ({e})\n", f.source, f.status)),
                    None => out.push_str(&format!(
                        "{} : {} ({} / {}、{} を埋めた)\n",
                        f.source,
                        f.status,
                        bytes(f.written),
                        bytes(f.size),
                        bytes(f.missing)
                    )),
                }
            }
            out.push_str(
                "\n読めなかった部分はゼロで埋めてある。画像や動画なら Phase 5 の\n\
                 修復モジュールで開ける形に直せる場合がある。\n",
            );
        }

        if !self.warnings.is_empty() {
            out.push_str("\n注記\n----\n");
            for w in &self.warnings {
                out.push_str(&format!("{w}\n"));
            }
        }
        out
    }
}

/// バイト数を人間向けに整形する。
///
/// テキストレポートは利用者がそのまま読むものなので、生の数字だけでは辛い。
/// 元の数字も要る場面のために、1KiB 以上は単位付きと生の値を並べて出す。
fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if value < 1024 {
        return format!("{value} バイト");
    }
    let mut v = value as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {} ({value} バイト)", UNITS[unit])
}

/// JSON 文字列としてエスケープする(前後の `"` を含む)。
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CopyReport {
        CopyReport {
            source: "/dev/disk4".to_string(),
            destination: PathBuf::from("/out"),
            summary: CopySummary {
                files: 2,
                copied: 1,
                partial: 1,
                bytes_expected: 300,
                bytes_written: 300,
                bytes_missing: 100,
                dirs: 1,
                ..CopySummary::default()
            },
            files: vec![
                FileResult {
                    source: "/a.txt".to_string(),
                    output: PathBuf::from("/out/a.txt"),
                    size: 100,
                    written: 100,
                    missing: 0,
                    read_errors: 0,
                    status: CopyStatus::Copied,
                    error: None,
                },
                FileResult {
                    source: "/b\"x\".bin".to_string(),
                    output: PathBuf::from("/out/b_x_.bin"),
                    size: 200,
                    written: 200,
                    missing: 100,
                    read_errors: 1,
                    status: CopyStatus::Partial,
                    error: None,
                },
            ],
            warnings: vec!["/link はシンボリックリンクなので飛ばした".to_string()],
        }
    }

    #[test]
    fn json_has_every_file_and_escapes_names() {
        let mut out = Vec::new();
        report().render_json(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(text.starts_with("{\n"), "{text}");
        assert!(text.contains("\"source\": \"/a.txt\""), "{text}");
        assert!(text.contains(r#"\"x\""#), "{text}");
        assert!(text.contains("\"status\": \"partial\""), "{text}");
        assert!(text.contains("\"complete\": false"), "{text}");
        // 配列の最後の要素にカンマを付けない。
        assert!(!text.contains("}},\n  ],"), "{text}");
    }

    #[test]
    fn text_summary_lists_broken_files() {
        let text = report().text_summary();
        assert!(text.contains("一部欠け:   1 件"), "{text}");
        assert!(text.contains("/b\"x\".bin"), "{text}");
        assert!(text.contains("シンボリックリンク"), "{text}");
    }

    #[test]
    fn summary_is_complete_only_without_losses() {
        let mut s = CopySummary {
            files: 1,
            copied: 1,
            ..CopySummary::default()
        };
        assert!(s.is_complete());
        s.partial = 1;
        assert!(!s.is_complete());
    }
}
