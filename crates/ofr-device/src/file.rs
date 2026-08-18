//! ファイルをバックエンドにした [`Device`]。
//!
//! ディスクイメージ(.img)の解析と、テストの両方で使う。
//! 開くのは読み取り専用ハンドルだけで、書き込み経路は持たない。

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::device::{Device, DeviceInfo, DeviceKind, clamp_read};
use crate::error::{DeviceError, Result, io_error};

/// 既定のブロックサイズ。イメージファイルのセクタサイズは分からないので 512 を仮定する。
pub const DEFAULT_BLOCK_SIZE: u32 = 512;

/// 読み取り専用のファイルバックエンド。
#[derive(Debug)]
pub struct FileDevice {
    file: File,
    path: PathBuf,
    len: u64,
    info: DeviceInfo,
}

impl FileDevice {
    /// イメージファイルを読み取り専用で開く。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_block_size(path, DEFAULT_BLOCK_SIZE)
    }

    /// ブロックサイズを指定してイメージファイルを開く。
    pub fn open_with_block_size(path: impl AsRef<Path>, block_size: u32) -> Result<Self> {
        assert!(block_size > 0, "block_size は 1 以上");
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => DeviceError::NotFound(path.display().to_string()),
            io::ErrorKind::PermissionDenied => DeviceError::PermissionDenied {
                path: path.clone(),
                source: e,
            },
            _ => io_error(0, 0, e),
        })?;
        let len = file.metadata().map_err(|e| io_error(0, 0, e))?.len();

        let display_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let mut info = DeviceInfo::new(
            path.display().to_string(),
            display_name,
            DeviceKind::ImageFile,
            len,
            block_size,
        );
        info.removable = false;

        Ok(Self {
            file,
            path,
            len,
            info,
        })
    }

    /// 元のパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Device for FileDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let Some(want) = clamp_read(offset, buf.len(), self.len) else {
            return Ok(0);
        };
        let buf = &mut buf[..want];

        let mut done = 0usize;
        while done < want {
            match pread(&self.file, &mut buf[done..], offset + done as u64) {
                Ok(0) => break, // 予期しない EOF。読めた分だけ返す。
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_error(offset + done as u64, want - done, e)),
            }
        }
        Ok(done)
    }

    fn len(&self) -> u64 {
        self.len
    }

    fn block_size(&self) -> u32 {
        self.info.block_size
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("ofr-device は Windows と macOS (unix) のみ対応 (PLAN.md 1章)");

/// シーク位置を持たない位置指定読み込み。`&File` で並行に呼べる。
#[cfg(unix)]
fn pread(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

#[cfg(windows)]
fn pread(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}
