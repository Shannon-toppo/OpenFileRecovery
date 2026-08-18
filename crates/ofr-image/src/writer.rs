//! イメージファイルの書き出し。
//!
//! 出力は単純な raw イメージ。**取得できた領域だけを書く**ので、
//! 未取得領域は穴(スパース)のまま残り、ファイルの実使用量は回収できた分で済む
//! (PLAN.md 5.2)。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::error::{ImageError, Result};

/// raw イメージの書き出し先。
#[derive(Debug)]
pub struct ImageWriter {
    file: File,
    path: PathBuf,
}

impl ImageWriter {
    /// 出力ファイルを開く。
    ///
    /// `resume` が真なら既存ファイルの内容を残したまま開く(再開用)。
    /// 偽なら中身を捨てて作り直す。どちらの場合も長さは `len` に設定するので、
    /// 書いていない領域はゼロが読める穴になる。
    pub fn create(path: &Path, len: u64, resume: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(!resume)
            .open(path)
            .map_err(|e| ImageError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        file.set_len(len).map_err(|e| ImageError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// 指定オフセットへ書く。
    pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let n = pwrite(&self.file, &buf[done..], offset + done as u64).map_err(|e| {
                ImageError::Io {
                    path: self.path.clone(),
                    source: e,
                }
            })?;
            if n == 0 {
                return Err(ImageError::Io {
                    path: self.path.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "書き込みが進まない(出力先の空き容量を確認すること)",
                    ),
                });
            }
            done += n;
        }
        Ok(())
    }

    /// OS のバッファを書き出す。
    pub fn sync(&self) -> Result<()> {
        self.file.sync_data().map_err(|e| ImageError::Io {
            path: self.path.clone(),
            source: e,
        })
    }

    /// 出力先のパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn pwrite(file: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

#[cfg(windows)]
fn pwrite(file: &File, buf: &[u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}
