//! 表示用の整形ヘルパ。

use std::time::Duration;

/// バイト数を人間向けに整形する(`1.5 GiB` など)。
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut v = value as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// 転送速度。
pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}

/// 経過時間を `HH:MM:SS` にする。
pub fn duration(d: Duration) -> String {
    let total = d.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// 推定残り時間。不明なら `--:--:--`。
pub fn eta(d: Option<Duration>) -> String {
    match d {
        Some(d) => duration(d),
        None => "--:--:--".to_string(),
    }
}

/// 表示幅が `width` になるよう右側を空白で埋める。
pub fn pad(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(display_width(s))));
    out
}

/// 表示幅が `width` になるよう左側を空白で埋める(右揃え)。
pub fn pad_left(s: &str, width: usize) -> String {
    let mut out = " ".repeat(width.saturating_sub(display_width(s)));
    out.push_str(s);
    out
}

/// 端末上の表示幅の目安。日本語などの全角文字を 2 桁として数える。
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if (c as u32) < 0x1100 { 1 } else { 2 })
        .sum()
}

/// JSON 文字列としてエスケープする(前後の `"` を含む)。
///
/// `ofr list --json` が出す項目はデバイス名など短い文字列だけなので、
/// JSON ライブラリを足さずにこれで済ませている。
pub fn json_string(s: &str) -> String {
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

    #[test]
    fn formats_byte_counts() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(200 * 1024), "200 KiB");
        assert_eq!(bytes(31_029_460_992), "28.9 GiB");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_secs(0)), "00:00:00");
        assert_eq!(duration(Duration::from_secs(3661)), "01:01:01");
        assert_eq!(eta(None), "--:--:--");
    }

    #[test]
    fn pads_to_display_width() {
        assert_eq!(pad("ID", 4), "ID  ");
        assert_eq!(pad("種別", 6), "種別  ");
        assert_eq!(pad("長すぎる名前", 2), "長すぎる名前");
        assert_eq!(pad_left("1.0 KiB", 10), "   1.0 KiB");
        assert_eq!(pad_left("容量", 10), "      容量");
    }

    #[test]
    fn counts_wide_characters() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("メモリ"), 6);
        assert_eq!(display_width("USB メモリ"), 4 + 6);
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(json_string("abc"), "\"abc\"");
        assert_eq!(json_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_string("改行\n"), "\"改行\\n\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }
}
