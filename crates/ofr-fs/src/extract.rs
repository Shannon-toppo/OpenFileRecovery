//! 見つかった項目を出力先へ書き出す。
//!
//! 復元は「デバイスから読んで、別のディスクへ書く」だけの単純な処理だが、
//! 相手が壊れかけメディアなので次の 2 点を守る:
//!
//! - 読めない所で止めない。リトライしたうえで、それでも駄目ならゼロで埋めて
//!   先へ進み、埋めたバイト数を記録する(PLAN.md 5.2 / 5.5)
//! - 出力先のファイル名は OS が受け付ける形に直す。FAT/exFAT には Windows で
//!   使えない名前(`CON`、末尾のドットなど)が入っていることがある

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use ofr_device::Device;

use crate::entry::RecoveredEntry;
use crate::error::{FsError, Result};

/// 復元の設定。
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// 読み込み失敗時のリトライ回数。
    pub retries: u32,
    /// リトライの待ち時間(指数バックオフの初期値)。
    pub retry_delay: Duration,
    /// 読み込み単位。
    pub chunk_size: usize,
    /// 読めなかった領域をゼロで埋めるか。偽なら、そこで打ち切る。
    pub zero_fill: bool,
    /// 元のタイムスタンプを出力ファイルに反映するか。
    pub set_timestamps: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            retries: 2,
            retry_delay: Duration::from_millis(100),
            chunk_size: 1 << 20,
            zero_fill: true,
            set_timestamps: true,
        }
    }
}

/// 1 ファイルの復元結果。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtractStats {
    /// 書き出したバイト数。
    pub written: u64,
    /// 読めずにゼロで埋めたバイト数。
    pub missing: u64,
    /// 読み込みエラーの回数。
    pub read_errors: u32,
}

impl ExtractStats {
    /// 全部読めたか。
    pub fn is_complete(&self) -> bool {
        self.missing == 0
    }
}

/// 項目のデータを `out` へ書き出す。
///
/// 書き出す長さは「記録されたサイズ」と「集められた領域の合計」の小さいほう。
pub fn extract_to_writer(
    device: &dyn Device,
    entry: &RecoveredEntry,
    out: &mut dyn Write,
    options: &ExtractOptions,
) -> Result<ExtractStats> {
    let mut stats = ExtractStats::default();
    let limit = entry.recoverable_bytes();
    if limit == 0 {
        return Ok(stats);
    }

    let chunk_size = options.chunk_size.max(512);
    let mut buf = vec![0u8; chunk_size];
    let mut remaining = limit;

    for extent in &entry.extents {
        if remaining == 0 {
            break;
        }
        let mut pos = extent.offset;
        let mut left = extent.len.min(remaining);
        while left > 0 {
            let want = left.min(chunk_size as u64) as usize;
            let target = &mut buf[..want];

            match read_with_retry(device, pos, target, options) {
                Ok(()) => {}
                Err(e) => {
                    stats.read_errors += 1;
                    if !options.zero_fill {
                        return Err(e.into());
                    }
                    tracing::debug!("offset {pos} の {want} バイトを読めない: {e}");
                    target.iter_mut().for_each(|b| *b = 0);
                    stats.missing += want as u64;
                }
            }

            out.write_all(target)
                .map_err(|e| FsError::output(PathBuf::from("<writer>"), e))?;
            stats.written += want as u64;
            pos += want as u64;
            left -= want as u64;
            remaining -= want as u64;
        }
    }

    Ok(stats)
}

/// 項目のデータをファイルへ書き出す。
///
/// 親ディレクトリは作らない(呼び出し側が作る)。
pub fn extract_to_path(
    device: &dyn Device,
    entry: &RecoveredEntry,
    path: &Path,
    options: &ExtractOptions,
) -> Result<ExtractStats> {
    let file = File::create(path).map_err(|e| FsError::output(path, e))?;
    let mut writer = BufWriter::new(file);
    let stats = extract_to_writer(device, entry, &mut writer, options)?;
    writer.flush().map_err(|e| FsError::output(path, e))?;

    let file = writer
        .into_inner()
        .map_err(|e| FsError::output(path, e.into_error()))?;

    if options.set_timestamps
        && let Some(modified) = entry.times.modified.and_then(|t| t.to_system_time())
    {
        // 失敗しても復元自体は成功なので、警告だけ出して続ける。
        if let Err(e) = file.set_modified(modified) {
            tracing::debug!("{} の更新日時を設定できない: {e}", path.display());
        }
    }
    Ok(stats)
}

/// ディレクトリを作る(既にあってもエラーにしない)。
pub fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|e| FsError::output(path, e))
}

fn read_with_retry(
    device: &dyn Device,
    offset: u64,
    buf: &mut [u8],
    options: &ExtractOptions,
) -> std::result::Result<(), ofr_device::DeviceError> {
    let mut delay = options.retry_delay;
    let mut attempt = 0;
    loop {
        match device.read_exact_at(offset, buf) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if attempt >= options.retries || !e.is_retryable() {
                    return Err(e);
                }
                attempt += 1;
                if !delay.is_zero() {
                    sleep(delay);
                }
                delay = (delay * 4).min(Duration::from_secs(2));
            }
        }
    }
}

/// Windows / macOS の両方で使えるファイル名に直す。
///
/// 使えない文字は `_` に置き換え、Windows の予約名は末尾に `_` を足して避ける。
/// 空になる場合は `_` を返す。
pub fn sanitize_component(name: &str) -> String {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();

    // Windows は末尾のドットと空白を落としてしまう。
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() || out == "." || out == ".." {
        return "_".to_string();
    }

    let stem = out.split('.').next().unwrap_or(&out).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// 既存ファイルとぶつからないパスを作る。
///
/// 削除済みファイルには同名のものが普通にあるので、`名前 (2).jpg` のように
/// 番号を足して避ける。
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (name, None),
    };
    for n in 2..10_000 {
        let candidate = match ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(name)
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;
    use crate::entry::{EntryKind, EntryStatus, Extent};

    fn entry(size: u64, extents: Vec<Extent>) -> RecoveredEntry {
        let mut e = RecoveredEntry::new("a.bin", EntryKind::File, EntryStatus::Deleted);
        e.size = size;
        e.extents = extents;
        e
    }

    #[test]
    fn writes_only_the_recorded_size() {
        let device = MockDevice::patterned(8192);
        let e = entry(
            100,
            vec![Extent {
                offset: 0,
                len: 4096,
            }],
        );
        let mut out = Vec::new();
        let stats = extract_to_writer(&device, &e, &mut out, &ExtractOptions::default()).unwrap();
        assert_eq!(stats.written, 100);
        assert_eq!(out.len(), 100);
        assert_eq!(out[0], MockDevice::pattern_byte(0));
        assert!(stats.is_complete());
    }

    #[test]
    fn joins_fragmented_extents_in_order() {
        let device = MockDevice::patterned(8192);
        let e = entry(
            8,
            vec![
                Extent {
                    offset: 4096,
                    len: 4,
                },
                Extent { offset: 0, len: 4 },
            ],
        );
        let mut out = Vec::new();
        extract_to_writer(&device, &e, &mut out, &ExtractOptions::default()).unwrap();
        assert_eq!(out[0], MockDevice::pattern_byte(4096));
        assert_eq!(out[4], MockDevice::pattern_byte(0));
    }

    #[test]
    fn zero_fills_unreadable_areas_and_keeps_going() {
        let device = MockDevice::builder(8192)
            .pattern()
            .bad_range(1024, 512)
            .build();
        let e = entry(
            2048,
            vec![Extent {
                offset: 0,
                len: 2048,
            }],
        );
        let options = ExtractOptions {
            chunk_size: 512,
            retries: 1,
            retry_delay: Duration::ZERO,
            ..ExtractOptions::default()
        };
        let mut out = Vec::new();
        let stats = extract_to_writer(&device, &e, &mut out, &options).unwrap();

        assert_eq!(stats.written, 2048);
        assert_eq!(stats.missing, 512);
        assert!(!stats.is_complete());
        assert_eq!(out[0], MockDevice::pattern_byte(0));
        assert_eq!(&out[1024..1536], &[0u8; 512]);
        assert_eq!(out[1536], MockDevice::pattern_byte(1536));
    }

    #[test]
    fn sanitizes_names_for_both_platforms() {
        assert_eq!(sanitize_component("a/b:c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_component("name."), "name");
        assert_eq!(sanitize_component("..."), "_");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(sanitize_component("con.txt"), "_con.txt");
        assert_eq!(sanitize_component("普通の名前.jpg"), "普通の名前.jpg");
    }

    #[test]
    fn avoids_overwriting_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.jpg"),
            dir.path().join("a (2).jpg")
        );
        assert_eq!(unique_path(dir.path(), "b.jpg"), dir.path().join("b.jpg"));
    }
}
