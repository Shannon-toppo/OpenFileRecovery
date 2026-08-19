//! マウント済みフォルダからのコピー(論理コピー)。
//!
//! OS がデバイスをマウントできている場合は、生読みより OS のファイル API で
//! 読むほうが速く、断片化も OS が解決してくれる(PLAN.md 5.5 の 1 番)。
//!
//! ただし相手は不安定なメディアなので、普通の再帰コピーとは 2 点違う:
//!
//! - 読み込み失敗はリトライし、それでも駄目な部分はゼロで埋めて先へ進む。
//!   1 ファイルの一部が読めないだけで全体を止めない
//! - シンボリックリンクは辿らない。リンク先がデバイスの外へ出ると、
//!   復旧と関係のないファイルまでコピーしてしまう

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

use ofr_fs::{EntryKind, EntryStatus, ExtractStats, extract::sanitize_component};

use crate::error::{CopyError, Result};
use crate::options::CopyOptions;
use crate::source::{CopyItem, CopySource};

/// マウント済みのフォルダをコピー元にする。
pub struct MountSource {
    root: PathBuf,
    /// 収集した項目の実パス。[`CopyItem::id`] が添字。
    ///
    /// 非 UTF-8 のファイル名(macOS / Linux では作れる)をパス文字列から
    /// 組み立て直すと壊れるので、実体はここに取っておく。
    paths: Mutex<Vec<PathBuf>>,
    warnings: Mutex<Vec<String>>,
}

impl MountSource {
    /// コピー元のフォルダを指定する。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            paths: Mutex::new(Vec::new()),
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// コピー元のルート。
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn warn(&self, message: impl Into<String>) {
        let message = message.into();
        tracing::debug!("{message}");
        self.warnings
            .lock()
            .expect("warnings poisoned")
            .push(message);
    }

    /// フォルダの中身を名前順に並べて返す。開けなければ警告して空を返す
    /// (1 つのフォルダを開けないだけで全体を止めない)。
    fn list_dir(&self, dir: &Path, prefix: &[OsString]) -> Vec<(PathBuf, Vec<OsString>)> {
        let read_dir = match fs::read_dir(dir) {
            Ok(r) => r,
            Err(e) => {
                self.warn(format!("{} を開けない: {e}", dir.display()));
                return Vec::new();
            }
        };

        let mut children = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(entry) => children.push(entry.path()),
                Err(e) => self.warn(format!("{} の項目を読めない: {e}", dir.display())),
            }
        }
        // 並び順を決めておく。同じフォルダなら毎回同じ結果になる。
        children.sort();

        children
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_os_string();
                let mut components = prefix.to_vec();
                components.push(name);
                Some((path, components))
            })
            .collect()
    }

    fn path_for(&self, item: &CopyItem) -> Result<PathBuf> {
        let paths = self.paths.lock().expect("paths poisoned");
        paths.get(item.id as usize).cloned().ok_or_else(|| {
            CopyError::source_io(
                &self.root,
                io::Error::other(format!("項目 {} を引けない", item.path)),
            )
        })
    }
}

impl CopySource for MountSource {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn collect(&self, cancel: &AtomicBool) -> Result<Vec<CopyItem>> {
        let meta = fs::metadata(&self.root).map_err(|e| CopyError::source_io(&self.root, e))?;
        if !meta.is_dir() {
            return Err(CopyError::source_io(
                &self.root,
                io::Error::new(io::ErrorKind::NotADirectory, "フォルダではない"),
            ));
        }

        let mut paths = Vec::new();
        let mut items = Vec::new();
        // 深さ優先・名前順で辿る。親を先に取り出してから、その子をスタックの
        // 一番上へ積むので、出来上がる列はそのままディレクトリ作成順になる。
        let mut stack: Vec<(PathBuf, Vec<OsString>)> = self.list_dir(&self.root, &[]);
        stack.reverse();

        while let Some((path, components)) = stack.pop() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    self.warn(format!("{} の情報を取れない: {e}", path.display()));
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                // リンク先がデバイスの外へ出ると、復旧と関係のないファイルまで
                // コピーしてしまう。辿らない。
                self.warn(format!(
                    "{} はシンボリックリンクなので飛ばした",
                    path.display()
                ));
                continue;
            }
            if !meta.is_dir() && !meta.is_file() {
                self.warn(format!(
                    "{} は通常のファイルではないので飛ばした",
                    path.display()
                ));
                continue;
            }

            let id = paths.len() as u64;
            paths.push(path.clone());
            items.push(CopyItem {
                path: display_path(&components),
                components: components
                    .iter()
                    .map(|c| sanitize_component(&c.to_string_lossy()))
                    .collect(),
                kind: if meta.is_dir() {
                    EntryKind::Dir
                } else {
                    EntryKind::File
                },
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok(),
                status: EntryStatus::Intact,
                id,
            });

            if meta.is_dir() {
                let mut children = self.list_dir(&path, &components);
                children.reverse();
                stack.extend(children);
            }
        }

        *self.paths.lock().expect("paths poisoned") = paths;
        Ok(items)
    }

    fn copy_file(
        &self,
        item: &CopyItem,
        output: &Path,
        options: &CopyOptions,
    ) -> Result<ExtractStats> {
        let src = self.path_for(item)?;
        let mut input = File::open(&src).map_err(|e| CopyError::source_io(&src, e))?;
        let out = File::create(output).map_err(|e| CopyError::output(output, e))?;
        let mut writer = BufWriter::new(out);

        let chunk_size = options.chunk_size.max(512);
        let mut buf = vec![0u8; chunk_size];
        let mut stats = ExtractStats::default();
        let mut pos = 0u64;

        while pos < item.size {
            let want = (item.size - pos).min(chunk_size as u64) as usize;
            match read_with_retry(&mut input, pos, &mut buf[..want], options) {
                Ok(read) => {
                    writer
                        .write_all(&buf[..read])
                        .map_err(|e| CopyError::output(output, e))?;
                    stats.written += read as u64;
                    pos += read as u64;
                    if read < want {
                        break; // 記録されたサイズより短かった。
                    }
                }
                Err(e) => {
                    stats.read_errors += 1;
                    if !options.zero_fill {
                        return Err(CopyError::source_io(&src, e));
                    }
                    tracing::debug!(
                        "{} の {pos} から {want} バイトを読めない: {e}",
                        src.display()
                    );
                    buf[..want].iter_mut().for_each(|b| *b = 0);
                    writer
                        .write_all(&buf[..want])
                        .map_err(|e| CopyError::output(output, e))?;
                    stats.written += want as u64;
                    stats.missing += want as u64;
                    pos += want as u64;
                }
            }
        }

        writer.flush().map_err(|e| CopyError::output(output, e))?;
        let file = writer
            .into_inner()
            .map_err(|e| CopyError::output(output, e.into_error()))?;
        if options.set_timestamps
            && let Some(modified) = item.modified
            && let Err(e) = file.set_modified(modified)
        {
            // 日時を移せなくてもコピー自体は成功。
            tracing::debug!("{} の更新日時を設定できない: {e}", output.display());
        }
        Ok(stats)
    }

    fn check_destination(&self, dest: &Path) -> Result<()> {
        // 宛先が復旧元の中にあると、書いたそばから自分でコピーし直すことになる。
        let root = resolve(&self.root);
        let dest = resolve(dest);
        if dest.starts_with(&root) {
            return Err(CopyError::DestinationInsideSource {
                root: self.root.clone(),
                dest,
            });
        }
        Ok(())
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings.lock().expect("warnings poisoned").clone()
    }
}

/// 表示用のパス(`/` 区切り)。
fn display_path(components: &[OsString]) -> String {
    let mut out = String::new();
    for c in components {
        out.push('/');
        out.push_str(&c.to_string_lossy());
    }
    out
}

/// まだ存在しないパスも、実在する一番近い親まで辿って正規化する。
///
/// `/Volumes/USB/../USB/out` のような書き方や、macOS の `/tmp` →
/// `/private/tmp` のようなシンボリックリンクを解いてから比べるために要る。
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };

    let mut current = absolute.clone();
    let mut rest: Vec<OsString> = Vec::new();
    loop {
        if let Ok(canonical) = fs::canonicalize(&current) {
            let mut out = canonical;
            for name in rest.iter().rev() {
                out.push(name);
            }
            return out;
        }
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return absolute;
        };
        if parent == current {
            return absolute;
        }
        match current.file_name() {
            Some(name) => rest.push(name.to_os_string()),
            None => return absolute,
        }
        current = parent;
    }
}

/// 1 チャンク読む。失敗したら指数バックオフを挟んで再試行する。
fn read_with_retry(
    file: &mut File,
    pos: u64,
    buf: &mut [u8],
    options: &CopyOptions,
) -> io::Result<usize> {
    let mut delay = options.retry_delay;
    let mut attempt = 0;
    loop {
        match read_at(file, pos, buf) {
            Ok(n) => return Ok(n),
            Err(e) => {
                if attempt >= options.retries {
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

fn read_at(file: &mut File, pos: u64, buf: &mut [u8]) -> io::Result<usize> {
    file.seek(SeekFrom::Start(pos))?;
    let mut done = 0;
    while done < buf.len() {
        match file.read(&mut buf[done..]) {
            Ok(0) => break, // ファイル末尾。
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, data: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn collects_the_tree_parents_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        write(&root.join("DCIM/100MSDCF/a.jpg"), b"aaa");
        write(&root.join("readme.txt"), b"hello");
        fs::create_dir_all(root.join("EMPTY")).unwrap();

        let source = MountSource::new(&root);
        let items = source.collect(&AtomicBool::new(false)).unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/DCIM",
                "/DCIM/100MSDCF",
                "/DCIM/100MSDCF/a.jpg",
                "/EMPTY",
                "/readme.txt",
            ]
        );
        assert_eq!(items[2].size, 3);
        assert!(items[3].is_dir());
    }

    #[test]
    fn copies_contents_and_keeps_the_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        write(&root.join("a.bin"), &[7u8; 5000]);

        let source = MountSource::new(&root);
        let items = source.collect(&AtomicBool::new(false)).unwrap();
        let out = dir.path().join("a.bin");
        let options = CopyOptions {
            chunk_size: 512,
            ..CopyOptions::default()
        };
        let stats = source.copy_file(&items[0], &out, &options).unwrap();

        assert_eq!(stats.written, 5000);
        assert!(stats.is_complete());
        assert_eq!(fs::read(&out).unwrap(), vec![7u8; 5000]);
        assert_eq!(
            fs::metadata(&out).unwrap().modified().unwrap(),
            fs::metadata(root.join("a.bin"))
                .unwrap()
                .modified()
                .unwrap()
        );
    }

    #[test]
    fn refuses_a_destination_inside_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        fs::create_dir_all(&root).unwrap();
        let source = MountSource::new(&root);

        assert!(source.check_destination(&root.join("out")).is_err());
        assert!(source.check_destination(&root).is_err());
        assert!(source.check_destination(&dir.path().join("out")).is_ok());
    }

    #[test]
    fn skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        write(&root.join("a.txt"), b"a");
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"x").unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_file(&outside, root.join("link.txt"));

        let source = MountSource::new(&root);
        let items = source.collect(&AtomicBool::new(false)).unwrap();
        assert!(items.iter().all(|i| i.path != "/link.txt"));
        #[cfg(unix)]
        assert!(source.warnings().iter().any(|w| w.contains("シンボリック")));
    }
}
