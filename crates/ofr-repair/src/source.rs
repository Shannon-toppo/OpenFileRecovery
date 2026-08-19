//! 修復対象ファイルの読み出し口。
//!
//! 修復は**元ファイルを絶対に書き換えない**(PLAN.md 5.6「修復は必ずコピーに対して
//! 行い、元ファイルは残す」)。入力を [`ofr_device::FileDevice`] 経由で開いているのは
//! そのためで、[`ofr_device::Device`] には書き込み API が存在しない。
//!
//! JPEG / PNG は全体をメモリに載せて弄る(写真のサイズなら問題にならない)。
//! AVI / MP4 は数 GiB になりうるので、ヘッダを窓読みで走査し、本体は
//! [`Source::copy_range`] で入力から出力へ直接流す。

use std::io::{self, Write};
use std::path::Path;

use ofr_device::{Device, FileDevice};

use crate::error::{RepairError, Result};

/// 窓の既定サイズ。ヘッダ走査はここに収まる。
const WINDOW: usize = 256 * 1024;

/// 読み取り専用のファイル入力。
pub(crate) struct Source {
    device: FileDevice,
    len: u64,
    buf: Vec<u8>,
    /// `buf[0]` が対応するファイル上の位置。
    start: u64,
    /// `buf` のうち実際に読めているバイト数。
    filled: usize,
}

impl Source {
    /// ファイルを読み取り専用で開く。
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let device = FileDevice::open(path).map_err(|e| match e {
            // 「デバイスが見つからない」はこの文脈では分かりにくい。
            // 修復が相手にするのはデバイスではなくファイル。
            ofr_device::DeviceError::NotFound(_) => RepairError::NotFound(path.to_path_buf()),
            other => RepairError::Input {
                path: path.to_path_buf(),
                source: other,
            },
        })?;
        let len = device.len();
        Ok(Self {
            device,
            len,
            buf: Vec::new(),
            start: 0,
            filled: 0,
        })
    }

    /// ファイル全長。
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// `offset` から最大 `want` バイトを見る。
    ///
    /// 返り値は要求より短いことがある(ファイル末尾 / 窓の上限 / 読み込みエラー)。
    /// 呼び出し側は必ず長さを確認すること(PLAN.md 6章 5項)。
    pub(crate) fn view(&mut self, offset: u64, want: usize) -> &[u8] {
        if want == 0 || offset >= self.len {
            return &[];
        }
        let want = want.min(WINDOW);
        let want = ((self.len - offset).min(want as u64)) as usize;

        let covered_end = self.start + self.filled as u64;
        let inside = offset >= self.start && offset < covered_end;
        if !(inside && offset + want as u64 <= covered_end) {
            self.refill(offset);
        }

        let Some(rel) = offset.checked_sub(self.start) else {
            return &[];
        };
        let rel = rel as usize;
        if rel >= self.filled {
            return &[];
        }
        let end = (rel + want).min(self.filled);
        &self.buf[rel..end]
    }

    /// `offset` を先頭に窓を張り直す。
    fn refill(&mut self, offset: u64) {
        if self.buf.len() < WINDOW {
            self.buf.resize(WINDOW, 0);
        }
        let want = ((self.len - offset).min(WINDOW as u64)) as usize;
        self.start = offset;
        self.filled = match self.device.read_at(offset, &mut self.buf[..want]) {
            Ok(n) => n,
            Err(e) => {
                // 壊れたメディアから直接修復することもある。読めない所は
                // 「そこで終わり」として扱い、修復自体は続ける。
                tracing::debug!(offset, error = %e, "入力を読めなかった");
                0
            }
        };
    }

    /// ちょうど `N` バイト読む。足りなければ `None`。
    pub(crate) fn array<const N: usize>(&mut self, offset: u64) -> Option<[u8; N]> {
        let v = self.view(offset, N);
        if v.len() < N {
            return None;
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&v[..N]);
        Some(out)
    }

    /// 1 バイト読む。
    pub(crate) fn u8(&mut self, offset: u64) -> Option<u8> {
        self.array::<1>(offset).map(|a| a[0])
    }

    /// ビッグエンディアン u32。
    pub(crate) fn u32be(&mut self, offset: u64) -> Option<u32> {
        self.array::<4>(offset).map(u32::from_be_bytes)
    }

    /// ビッグエンディアン u64。
    pub(crate) fn u64be(&mut self, offset: u64) -> Option<u64> {
        self.array::<8>(offset).map(u64::from_be_bytes)
    }

    /// リトルエンディアン u32。
    pub(crate) fn u32le(&mut self, offset: u64) -> Option<u32> {
        self.array::<4>(offset).map(u32::from_le_bytes)
    }

    /// `offset` から始まるバイト列が `expect` と一致するか。
    pub(crate) fn matches(&mut self, offset: u64, expect: &[u8]) -> bool {
        let v = self.view(offset, expect.len());
        v.len() == expect.len() && v == expect
    }

    /// `offset` から `len` バイトを取り出す。読めた分だけ返る。
    pub(crate) fn read_vec(&mut self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len.min(1 << 20));
        let mut at = offset;
        while out.len() < len {
            let chunk = self.view(at, len - out.len());
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(chunk);
            at += chunk.len() as u64;
        }
        out
    }

    /// ファイル全体をメモリに読み込む。
    ///
    /// `limit` を超えるファイルは弾く。JPEG / PNG しか通さないので、
    /// ここに来る時点で数百 MiB を超えていたら形式判定の方が間違っている。
    pub(crate) fn read_all(&mut self, limit: u64) -> Result<Vec<u8>> {
        if self.len > limit {
            return Err(RepairError::TooLarge {
                size: self.len,
                limit,
            });
        }
        Ok(self.read_vec(0, self.len as usize))
    }

    /// `offset` から `len` バイトを `out` へ流す。書けたバイト数を返す。
    ///
    /// 入力が途中で尽きた場合は読めた分だけ書いて返る(呼び出し側が
    /// 切り詰めを検出できるように、要求より少ない値になる)。
    pub(crate) fn copy_range(
        &mut self,
        out: &mut dyn Write,
        offset: u64,
        len: u64,
    ) -> io::Result<u64> {
        let mut done = 0u64;
        let mut at = offset;
        while done < len {
            let want = (len - done).min(WINDOW as u64) as usize;
            let chunk = self.view(at, want);
            if chunk.is_empty() {
                break;
            }
            out.write_all(chunk)?;
            done += chunk.len() as u64;
            at += chunk.len() as u64;
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(data: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(data).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn reads_across_the_window_boundary() {
        let data: Vec<u8> = (0..WINDOW * 2 + 100).map(|i| (i % 251) as u8).collect();
        let f = temp_file(&data);
        let mut src = Source::open(f.path()).unwrap();

        assert_eq!(src.len(), data.len() as u64);
        assert_eq!(src.u8(0), Some(0));
        let at = WINDOW as u64 - 2;
        assert_eq!(src.read_vec(at, 8), data[at as usize..at as usize + 8]);
        // 窓を戻しても同じものが読める。
        assert_eq!(src.u8(3), Some(3));
    }

    #[test]
    fn stops_at_end_of_file() {
        let f = temp_file(b"abcd");
        let mut src = Source::open(f.path()).unwrap();
        assert_eq!(src.read_vec(2, 100), b"cd");
        assert_eq!(src.u32be(4), None);
        assert!(src.view(10, 4).is_empty());
    }

    #[test]
    fn copies_a_range() {
        let f = temp_file(b"0123456789");
        let mut src = Source::open(f.path()).unwrap();
        let mut out = Vec::new();
        assert_eq!(src.copy_range(&mut out, 3, 4).unwrap(), 4);
        assert_eq!(out, b"3456");

        // 要求が末尾を越えたら読めた分だけ返る。
        out.clear();
        assert_eq!(src.copy_range(&mut out, 8, 10).unwrap(), 2);
        assert_eq!(out, b"89");
    }

    #[test]
    fn rejects_oversized_input() {
        let f = temp_file(b"0123456789");
        let mut src = Source::open(f.path()).unwrap();
        assert!(matches!(
            src.read_all(4),
            Err(RepairError::TooLarge { size: 10, limit: 4 })
        ));
    }
}
