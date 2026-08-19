//! 修復レポート。
//!
//! PLAN.md 5.6 の「Report には何をどこまで直せたか、検証結果、残っている問題を含める」
//! がこれ。JSON は GUI と後処理が読むためのもので、テキストは利用者がそのまま読む。
//! 復旧ソフトのレポートは「直った」より「何が失われたか」が肝心なので、
//! 直せなかったことを省略しない。
//!
//! JSON は手書きで組み立てる(ofr-copy のレポートと同じ理由で、この構造に
//! serde を足す意味がない)。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{RepairError, Result};
use crate::format::RepairFormat;

/// 修復の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairStatus {
    /// 元から壊れていなかった。
    Intact,
    /// 直せて、検証も通った。
    Repaired,
    /// 直したが完全ではない。欠けた部分を埋めてあるか、検証が通っていない。
    Partial,
    /// 直せなかった。
    Failed,
}

impl RepairStatus {
    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            RepairStatus::Intact => "壊れていない",
            RepairStatus::Repaired => "修復した",
            RepairStatus::Partial => "一部だけ修復した",
            RepairStatus::Failed => "修復できなかった",
        }
    }

    /// JSON に出す機械可読な名前。
    pub fn as_str(self) -> &'static str {
        match self {
            RepairStatus::Intact => "intact",
            RepairStatus::Repaired => "repaired",
            RepairStatus::Partial => "partial",
            RepairStatus::Failed => "failed",
        }
    }

    /// 使えるファイルが出力されたか。
    pub fn produced_output(self) -> bool {
        !matches!(self, RepairStatus::Failed)
    }

    /// まだ何も直していない (Intact) なら「直した」に上げる。
    ///
    /// 修復は複数の直しが積み重なるので、既に Partial まで落ちているものを
    /// Repaired に戻さないためのもの。
    pub(crate) fn or_repaired(self) -> RepairStatus {
        match self {
            RepairStatus::Intact => RepairStatus::Repaired,
            other => other,
        }
    }
}

impl std::fmt::Display for RepairStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// 修復結果の検証(PLAN.md 5.6)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verification {
    /// デコードが通った。静止画のみ。
    Decoded {
        /// デコードできた画像の幅。
        width: u32,
        /// デコードできた画像の高さ。
        height: u32,
    },
    /// コンテナ整合性チェックが通った。
    ///
    /// 動画の自動検証はここまで(全サンプルが本体の範囲内を指しているか等)。
    /// 実際に再生できるかは人間がプレイヤーで確かめる。
    Container,
    /// 検証が通らなかった。
    Failed(String),
    /// 検証していない。
    Skipped(String),
}

impl Verification {
    /// 検証が通ったか。
    pub fn passed(&self) -> bool {
        matches!(self, Verification::Decoded { .. } | Verification::Container)
    }

    /// 画面表示用の一行。
    pub fn label(&self) -> String {
        match self {
            Verification::Decoded { width, height } => {
                format!("デコード成功 ({width}x{height})")
            }
            Verification::Container => "コンテナ整合性 OK (再生確認は人間が行うこと)".to_string(),
            Verification::Failed(why) => format!("失敗: {why}"),
            Verification::Skipped(why) => format!("検証なし: {why}"),
        }
    }

    /// JSON に出す機械可読な名前。
    pub fn as_str(&self) -> &'static str {
        match self {
            Verification::Decoded { .. } => "decoded",
            Verification::Container => "container",
            Verification::Failed(_) => "failed",
            Verification::Skipped(_) => "skipped",
        }
    }
}

/// 1 ファイルの修復結果。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RepairReport {
    /// 修復元。中身は書き換えていない。
    pub input: PathBuf,
    /// 書き出した修復結果。失敗した場合は `None`。
    pub output: Option<PathBuf>,
    /// 使った参照ファイル。
    pub reference: Option<PathBuf>,
    /// 形式。
    pub format: RepairFormat,
    /// 結果。
    pub status: RepairStatus,
    /// 入力のサイズ。
    pub input_size: u64,
    /// 出力のサイズ。
    pub output_size: u64,
    /// 直したこと。利用者に見せる順で並ぶ。
    pub fixes: Vec<String>,
    /// 直しきれずに残っている問題。
    pub issues: Vec<String>,
    /// 検証結果。
    pub verification: Verification,
    /// 所要時間。
    pub elapsed: Duration,
}

impl RepairReport {
    /// 形式だけ決まった空のレポートを作る。
    pub(crate) fn new(input: &Path, format: RepairFormat, input_size: u64) -> Self {
        Self {
            input: input.to_path_buf(),
            output: None,
            reference: None,
            format,
            status: RepairStatus::Failed,
            input_size,
            output_size: 0,
            fixes: Vec::new(),
            issues: Vec::new(),
            verification: Verification::Skipped("修復に至らなかった".to_string()),
            elapsed: Duration::ZERO,
        }
    }

    /// 直したことを 1 件足す。
    pub(crate) fn fixed(&mut self, what: impl Into<String>) {
        self.fixes.push(what.into());
    }

    /// 残っている問題を 1 件足す。
    pub(crate) fn issue(&mut self, what: impl Into<String>) {
        self.issues.push(what.into());
    }

    /// 手を入れずに済んだか。
    pub fn is_intact(&self) -> bool {
        self.status == RepairStatus::Intact
    }

    /// JSON レポートを書き出す。
    pub fn write_json(&self, path: &Path) -> Result<()> {
        let file = File::create(path).map_err(|e| RepairError::output(path, e))?;
        let mut out = BufWriter::new(file);
        self.render_json(&mut out)
            .map_err(|e| RepairError::output(path, e))
    }

    /// JSON を書き出す。
    pub fn render_json(&self, out: &mut dyn Write) -> std::io::Result<()> {
        writeln!(out, "{{")?;
        writeln!(
            out,
            "  \"input\": {},",
            json_string(&self.input.display().to_string())
        )?;
        writeln!(
            out,
            "  \"output\": {},",
            match &self.output {
                Some(p) => json_string(&p.display().to_string()),
                None => "null".to_string(),
            }
        )?;
        writeln!(
            out,
            "  \"reference\": {},",
            match &self.reference {
                Some(p) => json_string(&p.display().to_string()),
                None => "null".to_string(),
            }
        )?;
        writeln!(out, "  \"format\": {},", json_string(self.format.as_str()))?;
        writeln!(out, "  \"status\": {},", json_string(self.status.as_str()))?;
        writeln!(out, "  \"input_size\": {},", self.input_size)?;
        writeln!(out, "  \"output_size\": {},", self.output_size)?;
        write_array(out, "fixes", &self.fixes)?;
        write_array(out, "issues", &self.issues)?;
        writeln!(
            out,
            "  \"verification\": {{\"result\": {}, \"detail\": {}}},",
            json_string(self.verification.as_str()),
            json_string(&self.verification.label())
        )?;
        writeln!(out, "  \"elapsed_ms\": {}", self.elapsed.as_millis())?;
        writeln!(out, "}}")?;
        out.flush()
    }

    /// 人間向けサマリ。CLI の画面出力でもある。
    pub fn text_summary(&self) -> String {
        let mut out = String::new();
        out.push_str("Open File Recovery 修復結果\n");
        out.push_str("===========================\n\n");
        out.push_str(&format!("修復元: {}\n", self.input.display()));
        match &self.output {
            Some(p) => out.push_str(&format!("出力:   {}\n", p.display())),
            None => out.push_str("出力:   なし\n"),
        }
        if let Some(r) = &self.reference {
            out.push_str(&format!("参照:   {}\n", r.display()));
        }
        out.push_str(&format!("形式:   {}\n", self.format));
        out.push_str(&format!("結果:   {}\n", self.status));
        out.push_str(&format!(
            "サイズ: {} → {}\n",
            bytes(self.input_size),
            bytes(self.output_size)
        ));
        out.push_str(&format!("検証:   {}\n", self.verification.label()));

        if !self.fixes.is_empty() {
            out.push_str("\n直したこと\n----------\n");
            for f in &self.fixes {
                out.push_str(&format!("- {f}\n"));
            }
        }
        if !self.issues.is_empty() {
            out.push_str("\n残っている問題\n--------------\n");
            for i in &self.issues {
                out.push_str(&format!("- {i}\n"));
            }
        }
        out.push_str(
            "\n修復元は書き換えていない。結果に納得できなければ元のファイルからやり直せる。\n",
        );
        out
    }
}

/// 文字列配列を 1 項目として書く。
fn write_array(out: &mut dyn Write, name: &str, items: &[String]) -> std::io::Result<()> {
    if items.is_empty() {
        return writeln!(out, "  \"{name}\": [],");
    }
    writeln!(out, "  \"{name}\": [")?;
    for (i, item) in items.iter().enumerate() {
        let comma = if i + 1 == items.len() { "" } else { "," };
        writeln!(out, "    {}{comma}", json_string(item))?;
    }
    writeln!(out, "  ],")
}

/// バイト数を人間向けに整形する。
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

    fn report() -> RepairReport {
        let mut r = RepairReport::new(Path::new("/in/broken.jpg"), RepairFormat::Jpeg, 1000);
        r.output = Some(PathBuf::from("/out/fixed.jpg"));
        r.output_size = 1200;
        r.status = RepairStatus::Partial;
        r.fixed("EOI マーカーを付け直した");
        r.issue("末尾 30% が失われている。グレーで埋めた");
        r.verification = Verification::Decoded {
            width: 640,
            height: 480,
        };
        r
    }

    #[test]
    fn json_lists_fixes_and_issues() {
        let mut out = Vec::new();
        report().render_json(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(text.starts_with("{\n"), "{text}");
        assert!(text.contains("\"status\": \"partial\""), "{text}");
        assert!(text.contains("EOI マーカー"), "{text}");
        assert!(text.contains("\"result\": \"decoded\""), "{text}");
        // 配列の最後の要素にカンマを付けない。
        assert!(!text.contains("\",\n  ],"), "{text}");
    }

    #[test]
    fn empty_arrays_stay_valid_json() {
        let mut out = Vec::new();
        RepairReport::new(Path::new("x.png"), RepairFormat::Png, 0)
            .render_json(&mut out)
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"fixes\": [],"), "{text}");
        assert!(text.contains("\"issues\": [],"), "{text}");
    }

    #[test]
    fn text_summary_mentions_the_original_is_kept() {
        let text = report().text_summary();
        assert!(text.contains("一部だけ修復した"), "{text}");
        assert!(text.contains("修復元は書き換えていない"), "{text}");
    }
}
