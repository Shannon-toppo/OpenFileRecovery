//! GNU ddrescue 互換の mapfile。
//!
//! 中断・再開はこのファイルだけで実現する。本家 ddrescue と行き来できるように
//! 書式を合わせてある(PLAN.md 5.2)。書式は公開されているマニュアルの記述に基づく。
//!
//! ```text
//! # Mapfile. Created by Open File Recovery 0.0.1
//! # current_pos  current_status  current_pass
//! 0x00001000     ?               1
//! #      pos        size  status
//! 0x00000000  0x00001000  +
//! 0x00001000  0x00000200  -
//! ```

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::blocks::{Block, BlockList, BlockStatus};
use crate::error::{ImageError, Result};

/// mapfile 先頭の状態行が示す「いま何をしているか」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentStatus {
    /// 未試行領域をコピー中 (`?`)。
    Copying,
    /// トリム中 (`*`)。
    Trimming,
    /// スクレイプ中 (`/`)。
    Scraping,
    /// リトライ中 (`-`)。
    Retrying,
    /// 穴埋め中 (`F`)。本ソフトでは使わないが、読み込みは受け付ける。
    Filling,
    /// 生成中 (`G`)。同上。
    Generating,
    /// 完了 (`+`)。
    Finished,
}

impl CurrentStatus {
    /// mapfile 上の 1 文字表現。
    pub fn as_char(self) -> char {
        match self {
            CurrentStatus::Copying => '?',
            CurrentStatus::Trimming => '*',
            CurrentStatus::Scraping => '/',
            CurrentStatus::Retrying => '-',
            CurrentStatus::Filling => 'F',
            CurrentStatus::Generating => 'G',
            CurrentStatus::Finished => '+',
        }
    }

    /// 1 文字表現から復元する。
    pub fn from_char(c: char) -> Option<Self> {
        Some(match c {
            '?' => CurrentStatus::Copying,
            '*' => CurrentStatus::Trimming,
            '/' => CurrentStatus::Scraping,
            '-' => CurrentStatus::Retrying,
            'F' => CurrentStatus::Filling,
            'G' => CurrentStatus::Generating,
            '+' => CurrentStatus::Finished,
            _ => return None,
        })
    }
}

/// mapfile の中身。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapFile {
    /// 次に処理する位置。
    pub current_pos: u64,
    /// 現在の状態。
    pub current_status: CurrentStatus,
    /// 現在のパス番号(1 始まり)。
    pub current_pass: u32,
    /// 領域マップ。
    pub blocks: BlockList,
}

impl MapFile {
    /// 全域未試行の mapfile を作る。
    pub fn new(total: u64) -> Self {
        Self {
            current_pos: 0,
            current_status: CurrentStatus::Copying,
            current_pass: 1,
            blocks: BlockList::new(total),
        }
    }

    /// テキストから読み込む。
    ///
    /// 総サイズはブロック列の終端から決まる。デバイスサイズと違う場合は
    /// 呼び出し側(再開処理)が突き合わせる。
    pub fn parse(text: &str) -> Result<Self> {
        let mut current: Option<(u64, CurrentStatus, u32)> = None;
        let mut blocks: Vec<Block> = Vec::new();

        for (i, raw) in text.lines().enumerate() {
            let line_no = i + 1;
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();

            if current.is_none() {
                // 最初の非コメント行は状態行。pass は ddrescue 1.20 以降の追加なので任意。
                if fields.len() < 2 {
                    return Err(map_error(line_no, "状態行の項目が足りない"));
                }
                let pos = parse_u64(fields[0])
                    .ok_or_else(|| map_error(line_no, "current_pos が数値でない"))?;
                let status = single_char(fields[1])
                    .and_then(CurrentStatus::from_char)
                    .ok_or_else(|| map_error(line_no, "current_status が不正"))?;
                let pass = fields
                    .get(2)
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(1);
                current = Some((pos, status, pass));
                continue;
            }

            if fields.len() < 3 {
                return Err(map_error(line_no, "ブロック行の項目が足りない"));
            }
            let pos = parse_u64(fields[0]).ok_or_else(|| map_error(line_no, "pos が数値でない"))?;
            let size =
                parse_u64(fields[1]).ok_or_else(|| map_error(line_no, "size が数値でない"))?;
            let status = single_char(fields[2])
                .and_then(BlockStatus::from_char)
                .ok_or_else(|| map_error(line_no, "status が不正"))?;
            blocks.push(Block { pos, size, status });
        }

        let Some((current_pos, current_status, current_pass)) = current else {
            return Err(map_error(0, "mapfile が空"));
        };
        let total = blocks.iter().map(Block::end).max().unwrap_or(0);

        Ok(Self {
            current_pos,
            current_status,
            current_pass,
            blocks: BlockList::from_blocks(total, blocks),
        })
    }

    /// ファイルから読み込む。
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|e| ImageError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&text)
    }

    /// ファイルへ書き出す。
    ///
    /// 一時ファイルへ書いてから rename する。書き込み中に電源が落ちても
    /// 前回の mapfile が壊れないようにするため。
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp: PathBuf = path.with_extension(match path.extension() {
            Some(ext) => format!("{}.tmp", ext.to_string_lossy()),
            None => "tmp".to_string(),
        });
        fs::write(&tmp, self.to_text()).map_err(|e| ImageError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        fs::rename(&tmp, path).map_err(|e| ImageError::Io {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// ddrescue 互換のテキスト表現。
    pub fn to_text(&self) -> String {
        let mut s = String::with_capacity(128 + self.blocks.blocks().len() * 32);
        let _ = writeln!(
            s,
            "# Mapfile. Created by Open File Recovery {}",
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(s, "# current_pos  current_status  current_pass");
        let _ = writeln!(
            s,
            "0x{:08X}     {}               {}",
            self.current_pos,
            self.current_status.as_char(),
            self.current_pass
        );
        let _ = writeln!(s, "#      pos        size  status");
        for b in self.blocks.blocks() {
            let _ = writeln!(
                s,
                "0x{:08X}  0x{:08X}  {}",
                b.pos,
                b.size,
                b.status.as_char()
            );
        }
        s
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// 16 進(`0x` 付き)と 10 進のどちらも受け付ける。
fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

fn map_error(line: usize, message: &str) -> ImageError {
    ImageError::MapFormat {
        line,
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GNU ddrescue 1.27 が実際に書く形の mapfile。
    const DDRESCUE_SAMPLE: &str = "\
# Mapfile. Created by GNU ddrescue version 1.27
# Command line: ddrescue -d /dev/sdb image.img image.map
# Start time:   2024-05-01 10:00:00
# Current time: 2024-05-01 10:03:21
# copying non-tried blocks... Pass 1 (forwards)
#      pos        size  status
0x00300000     ?               1
#      pos        size  status
0x00000000  0x00100000  +
0x00100000  0x00000200  -
0x00100200  0x00200000  ?
";

    #[test]
    fn parses_ddrescue_mapfiles() {
        let map = MapFile::parse(DDRESCUE_SAMPLE).unwrap();
        assert_eq!(map.current_pos, 0x0030_0000);
        assert_eq!(map.current_status, CurrentStatus::Copying);
        assert_eq!(map.current_pass, 1);
        assert_eq!(map.blocks.bytes_with(BlockStatus::Finished), 0x0010_0000);
        assert_eq!(map.blocks.bytes_with(BlockStatus::BadSector), 0x200);
        assert_eq!(map.blocks.bytes_with(BlockStatus::NonTried), 0x0020_0000);
        assert_eq!(map.blocks.total(), 0x0030_0200);
    }

    #[test]
    fn round_trips_through_text() {
        let mut map = MapFile::new(4096);
        map.blocks.mark(0, 1024, BlockStatus::Finished);
        map.blocks.mark(1024, 512, BlockStatus::BadSector);
        map.current_pos = 1536;
        map.current_status = CurrentStatus::Retrying;
        map.current_pass = 3;

        let parsed = MapFile::parse(&map.to_text()).unwrap();
        assert_eq!(parsed, map);
    }

    #[test]
    fn accepts_decimal_numbers() {
        let text = "1024 ? 1\n0 512 +\n512 512 -\n";
        let map = MapFile::parse(text).unwrap();
        assert_eq!(map.current_pos, 1024);
        assert_eq!(map.blocks.rescued(), 512);
    }

    #[test]
    fn rejects_broken_files() {
        assert!(MapFile::parse("").is_err());
        assert!(MapFile::parse("# コメントだけ\n").is_err());
        assert!(MapFile::parse("0x100 X 1\n").is_err());
        assert!(MapFile::parse("0x100 ? 1\nnotanumber 0x10 +\n").is_err());
        assert!(MapFile::parse("0x100 ? 1\n0x0 0x10\n").is_err());
    }

    #[test]
    fn saves_atomically_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.img.map");

        let mut map = MapFile::new(8192);
        map.blocks.mark(0, 4096, BlockStatus::Finished);
        map.save(&path).unwrap();

        let loaded = MapFile::load(&path).unwrap();
        assert_eq!(loaded.blocks.rescued(), 4096);
        // 一時ファイルは残らない。
        assert!(!path.with_extension("map.tmp").exists());
    }
}
