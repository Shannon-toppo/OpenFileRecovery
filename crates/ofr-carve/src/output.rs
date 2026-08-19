//! 切り出したファイルの命名と書き出し。
//!
//! カービングは元のファイル名を復元できない(PLAN.md 5.4)。名前は連番が基本で、
//! Exif などから撮影日時を拾えたものはそれを頭に付ける。
//!
//! 不良セクタに当たった場合は、その部分をゼロで埋めて残りを書き出す。
//! 「読めた分は保存する」が復旧ソフトとしての正しい振る舞いで、
//! 欠けた量は [`CarvedFile::bad_bytes`] に記録してレポートに出す。

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use ofr_device::Device;

use crate::error::{CarveError, Result};
use crate::fill;
use crate::format::FileMetadata;
use crate::result::CarvedFile;

/// 書き出しの読み込み単位。
const COPY_STEP: usize = 64 * 1024;

/// 切り出したファイルの名前を決める。
///
/// - 日時が取れた場合: `20230415-142530_000042.jpg`
/// - 取れなかった場合: `carved_000042.jpg`
pub(crate) fn file_name(index: u64, extension: &str, meta: &FileMetadata) -> String {
    match meta.timestamp.filter(|t| t.is_valid()) {
        Some(ts) => format!("{}_{index:06}.{extension}", ts.file_stamp()),
        None => format!("carved_{index:06}.{extension}"),
    }
}

/// 出力先ディレクトリ。
pub(crate) struct Writer {
    root: PathBuf,
    /// 真なら種類別サブフォルダを作らず、全部を直下に置く。
    flat: bool,
}

impl Writer {
    /// 出力先を用意する。
    pub(crate) fn create(root: &Path, flat: bool) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| CarveError::CreateDir {
            path: root.to_path_buf(),
            source: e,
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            flat,
        })
    }

    /// 出力先のルート。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 1 ファイルを書き出し、`file` の書き出し結果を埋める。
    pub(crate) fn write(&self, device: &dyn Device, file: &mut CarvedFile) -> Result<PathBuf> {
        let dir = if self.flat {
            self.root.clone()
        } else {
            self.root.join(file.extension)
        };
        if !self.flat {
            fs::create_dir_all(&dir).map_err(|e| CarveError::CreateDir {
                path: dir.clone(),
                source: e,
            })?;
        }
        let path = dir.join(&file.file_name);
        let out = File::create(&path).map_err(|e| CarveError::Write {
            path: path.clone(),
            source: e,
        })?;
        let mut out = BufWriter::with_capacity(COPY_STEP, out);

        let mut buf = vec![0u8; COPY_STEP];
        let mut done = 0u64;
        let mut bad = 0u64;
        while done < file.size {
            let n = ((file.size - done) as usize).min(COPY_STEP);
            let at = file.offset + done;
            // 不良セクタは 512 バイト単位まで粘って救い、駄目な所だけゼロで埋める。
            let result = fill::fill(device, at, &mut buf[..n], true);
            if result.filled == 0 {
                break; // デバイス末尾。
            }
            out.write_all(&buf[..result.filled])
                .map_err(|e| CarveError::Write {
                    path: path.clone(),
                    source: e,
                })?;
            done += result.filled as u64;
            bad += result.bad as u64;
            if result.filled < n {
                break; // デバイス末尾で切れた。
            }
        }
        out.flush().map_err(|e| CarveError::Write {
            path: path.clone(),
            source: e,
        })?;

        file.bytes_written = done.saturating_sub(bad);
        file.bad_bytes = bad;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Timestamp;

    #[test]
    fn names_files_by_index_and_timestamp() {
        let empty = FileMetadata::default();
        assert_eq!(file_name(42, "jpg", &empty), "carved_000042.jpg");

        let meta = FileMetadata {
            timestamp: Some(Timestamp {
                year: 2023,
                month: 4,
                day: 15,
                hour: 14,
                minute: 25,
                second: 30,
            }),
            ..FileMetadata::default()
        };
        assert_eq!(file_name(42, "jpg", &meta), "20230415-142530_000042.jpg");

        // 暦として成立しない日時は無視して連番だけにする。
        let broken = FileMetadata {
            timestamp: Some(Timestamp {
                year: 1601,
                month: 0,
                day: 0,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            ..FileMetadata::default()
        };
        assert_eq!(file_name(1, "png", &broken), "carved_000001.png");
    }
}
