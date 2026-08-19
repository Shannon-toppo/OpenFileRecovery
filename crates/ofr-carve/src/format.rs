//! 対応ファイル形式と、切り出し結果に付くメタデータ。

use std::fmt;

/// カービングが認識するファイル形式。
///
/// 拡張子は形式だけでは決まらない(ZIP は docx にも xlsx にもなり、
/// ISO-BMFF は mp4 / mov / heic に分かれる)ので、実際の拡張子は
/// [`CarvedFile::extension`](crate::CarvedFile::extension) が持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FileFormat {
    /// JPEG 画像。
    Jpeg,
    /// PNG 画像。
    Png,
    /// GIF 画像。
    Gif,
    /// HEIF / HEIC 画像(ISO-BMFF)。
    Heic,
    /// MP4 動画(ISO-BMFF)。
    Mp4,
    /// QuickTime 動画(ISO-BMFF)。
    Mov,
    /// AVI 動画(RIFF)。
    Avi,
    /// WAV 音声(RIFF)。
    Wav,
    /// MP3 音声。
    Mp3,
    /// ZIP 書庫(docx / xlsx / pptx を含む)。
    Zip,
    /// PDF 文書。
    Pdf,
}

impl FileFormat {
    /// 既定の拡張子(ドットなし)。
    pub fn default_extension(self) -> &'static str {
        match self {
            FileFormat::Jpeg => "jpg",
            FileFormat::Png => "png",
            FileFormat::Gif => "gif",
            FileFormat::Heic => "heic",
            FileFormat::Mp4 => "mp4",
            FileFormat::Mov => "mov",
            FileFormat::Avi => "avi",
            FileFormat::Wav => "wav",
            FileFormat::Mp3 => "mp3",
            FileFormat::Zip => "zip",
            FileFormat::Pdf => "pdf",
        }
    }

    /// CLI / 設定で使う名前。
    pub fn name(self) -> &'static str {
        match self {
            FileFormat::Jpeg => "jpeg",
            FileFormat::Heic => "heic",
            _ => self.default_extension(),
        }
    }

    /// 全形式。
    pub fn all() -> &'static [FileFormat] {
        &[
            FileFormat::Jpeg,
            FileFormat::Png,
            FileFormat::Gif,
            FileFormat::Heic,
            FileFormat::Mp4,
            FileFormat::Mov,
            FileFormat::Avi,
            FileFormat::Wav,
            FileFormat::Mp3,
            FileFormat::Zip,
            FileFormat::Pdf,
        ]
    }

    /// 名前から引く。拡張子表記(`jpg`, `docx` など)も受け付ける。
    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.trim().trim_start_matches('.').to_ascii_lowercase();
        Some(match n.as_str() {
            "jpeg" | "jpg" | "jpe" => FileFormat::Jpeg,
            "png" => FileFormat::Png,
            "gif" => FileFormat::Gif,
            "heic" | "heif" => FileFormat::Heic,
            "mp4" | "m4v" | "m4a" | "3gp" => FileFormat::Mp4,
            "mov" | "qt" => FileFormat::Mov,
            "avi" => FileFormat::Avi,
            "wav" | "wave" => FileFormat::Wav,
            "mp3" => FileFormat::Mp3,
            "zip" | "docx" | "xlsx" | "pptx" => FileFormat::Zip,
            "pdf" => FileFormat::Pdf,
            _ => return None,
        })
    }

    /// 出力を種類別フォルダに分けるときのフォルダ名。
    pub fn category(self) -> &'static str {
        match self {
            FileFormat::Jpeg | FileFormat::Png | FileFormat::Gif | FileFormat::Heic => "画像",
            FileFormat::Mp4 | FileFormat::Mov | FileFormat::Avi => "動画",
            FileFormat::Wav | FileFormat::Mp3 => "音声",
            FileFormat::Zip | FileFormat::Pdf => "文書",
        }
    }
}

impl fmt::Display for FileFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// 切り出した境界の確からしさ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// 終端マーカーやサイズ情報から終端を確定できた。
    Exact,
    /// 終端を確定できず、上限(次のシグネチャ位置か最大サイズ)で切った。
    Truncated,
}

impl Confidence {
    /// 終端を確定できたか。
    pub fn is_exact(self) -> bool {
        self == Confidence::Exact
    }

    /// 画面表示用の名前。
    pub fn label(self) -> &'static str {
        match self {
            Confidence::Exact => "完全",
            Confidence::Truncated => "推定",
        }
    }
}

impl fmt::Display for Confidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// 日時(タイムゾーンなしのカレンダー日時)。
///
/// Exif の `DateTimeOriginal` や ISO-BMFF の `mvhd` から拾う。
/// 日時ライブラリを足さずに済ませるため、必要最小限の項目だけ持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    /// 年(西暦)。
    pub year: i32,
    /// 月(1〜12)。
    pub month: u32,
    /// 日(1〜31)。
    pub day: u32,
    /// 時(0〜23)。
    pub hour: u32,
    /// 分(0〜59)。
    pub minute: u32,
    /// 秒(0〜59)。
    pub second: u32,
}

impl Timestamp {
    /// 各項目が暦として成立しているか。
    pub fn is_valid(&self) -> bool {
        (1900..=2200).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// ファイル名に使う `20230415-142530` 形式。
    pub fn file_stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Unix エポック(1970-01-01)からの秒数から作る。
    pub fn from_unix_seconds(secs: i64) -> Option<Self> {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days)?;
        Some(Self {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: ((rem % 3600) / 60) as u32,
            second: (rem % 60) as u32,
        })
    }

    /// `YYYY:MM:DD HH:MM:SS`(Exif の日時表記)を読む。
    pub fn parse_exif(text: &str) -> Option<Self> {
        let bytes = text.trim().as_bytes();
        if bytes.len() < 19 {
            return None;
        }
        let num = |r: std::ops::Range<usize>| -> Option<u32> {
            std::str::from_utf8(&bytes[r]).ok()?.trim().parse().ok()
        };
        let ts = Timestamp {
            year: num(0..4)? as i32,
            month: num(5..7)?,
            day: num(8..10)?,
            hour: num(11..13)?,
            minute: num(14..16)?,
            second: num(17..19)?,
        };
        ts.is_valid().then_some(ts)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// 1970-01-01 からの日数を年月日に直す。
///
/// Howard Hinnant の `civil_from_days` と同じ、グレゴリオ暦の素直な逆算。
fn civil_from_days(days: i64) -> Option<(i32, u32, u32)> {
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // 0..=399
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11 (3月始まり)
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = i32::try_from(if m <= 2 { y + 1 } else { y }).ok()?;
    Some((year, m as u32, d as u32))
}

/// 切り出したファイルから拾えたメタデータ。
///
/// 全項目が任意。取れなかったものは `None` のままにして、
/// 「取れなかった」と「値が 0」を区別できるようにする。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileMetadata {
    /// 撮影日時 / 作成日時。
    pub timestamp: Option<Timestamp>,
    /// 画素幅。
    pub width: Option<u32>,
    /// 画素高さ。
    pub height: Option<u32>,
    /// カメラのメーカー名(Exif `Make`)。
    pub camera_make: Option<String>,
    /// カメラの機種名(Exif `Model`)。
    pub camera_model: Option<String>,
    /// Exif の回転情報(1〜8)。
    pub orientation: Option<u16>,
    /// 再生時間(ミリ秒)。
    pub duration_ms: Option<u64>,
}

impl FileMetadata {
    /// 何か 1 つでも項目が埋まっているか。
    pub fn is_empty(&self) -> bool {
        *self == FileMetadata::default()
    }

    /// 埋まっている項目を `幅x高さ`, `2023-04-15 14:25:30` のような短い一覧にする。
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ts) = self.timestamp {
            parts.push(ts.to_string());
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            parts.push(format!("{w}x{h}"));
        }
        if let Some(ms) = self.duration_ms {
            parts.push(format!("{:.1}秒", ms as f64 / 1000.0));
        }
        match (&self.camera_make, &self.camera_model) {
            (Some(make), Some(model)) => parts.push(format!("{make} {model}")),
            (Some(s), None) | (None, Some(s)) => parts.push(s.clone()),
            (None, None) => {}
        }
        parts.join(" / ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_unix_seconds_to_calendar_dates() {
        let ts = Timestamp::from_unix_seconds(0).unwrap();
        assert_eq!(ts.to_string(), "1970-01-01 00:00:00");

        // 2023-04-15 14:25:30 UTC
        let ts = Timestamp::from_unix_seconds(1_681_568_730).unwrap();
        assert_eq!(ts.to_string(), "2023-04-15 14:25:30");
        assert_eq!(ts.file_stamp(), "20230415-142530");

        // うるう日をまたぐ。
        let ts = Timestamp::from_unix_seconds(951_782_400).unwrap();
        assert_eq!(ts.to_string(), "2000-02-29 00:00:00");
    }

    #[test]
    fn parses_exif_timestamps() {
        let ts = Timestamp::parse_exif("2023:04:15 14:25:30").unwrap();
        assert_eq!(ts.year, 2023);
        assert_eq!(ts.month, 4);
        assert_eq!(ts.day, 15);
        assert_eq!(ts.second, 30);

        // 未設定のフィールドはゼロ埋めや空白で来る。暦として不正なものは弾く。
        assert_eq!(Timestamp::parse_exif("0000:00:00 00:00:00"), None);
        assert_eq!(Timestamp::parse_exif("2023:04:15"), None);
        assert_eq!(Timestamp::parse_exif(""), None);
    }

    #[test]
    fn format_names_round_trip() {
        for f in FileFormat::all() {
            assert_eq!(FileFormat::from_name(f.name()), Some(*f));
            assert_eq!(FileFormat::from_name(f.default_extension()), Some(*f));
        }
        assert_eq!(FileFormat::from_name(".DOCX"), Some(FileFormat::Zip));
        assert_eq!(FileFormat::from_name("tiff"), None);
    }
}
