//! macOS の生デバイスアクセスと列挙。
//!
//! - 読み込みは `/dev/rdiskN`(raw デバイス)を `O_RDONLY` で開いて行う。
//!   raw デバイスはセクタ境界に整列した読み込みしか受け付けないので、
//!   整列は [`crate::align::read_via_bounce`] で吸収する(PLAN.md 5.1)。
//! - 列挙は `diskutil` の plist 出力を読む。`diskutil` が使えない環境では
//!   `/dev` の走査にフォールバックする。
//! - 起動ディスクは `statfs("/")` で判定し、APFS 合成ディスクの場合は
//!   その物理ストアも起動ディスク扱いにする(PLAN.md 6章 3項)。
//!
//! 生デバイスの読み込みには root 権限が必要。Phase 1 では `sudo ofr ...` で使う
//! 前提とし、GUI からの権限昇格は Phase 6 で扱う(PLAN.md 10章)。

// libc の FFI を使うのはこのモジュールだけ。
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use crate::align::{AlignedBuf, read_via_bounce};
use crate::device::{Device, DeviceInfo, DeviceKind};
use crate::error::{DeviceError, Result, classify_read_error};

mod plist;

/// バウンスバッファの大きさ。1 回の生読み込みの上限でもある。
const BOUNCE_SIZE: usize = 1 << 20;

/// バウンスバッファのアドレス整列。
const BOUNCE_ALIGN: usize = 4096;

/// 既定のセクタサイズ(ioctl が失敗したときの仮定値)。
const FALLBACK_BLOCK_SIZE: u32 = 512;

// _IOR('d', n, t) の展開。xnu の sys/ioccom.h と同じ計算。
const fn ior(group: u8, num: u8, size: usize) -> u64 {
    0x4000_0000 | (((size as u64) & 0x1fff) << 16) | ((group as u64) << 8) | (num as u64)
}

/// セクタサイズ(バイト)を取得する ioctl。
const DKIOCGETBLOCKSIZE: u64 = ior(b'd', 24, size_of::<u32>());
/// セクタ数を取得する ioctl。
const DKIOCGETBLOCKCOUNT: u64 = ior(b'd', 25, size_of::<u64>());
/// 1 回の読み込みで許される最大バイト数を取得する ioctl。
const DKIOCGETMAXBYTECOUNTREAD: u64 = ior(b'd', 70, size_of::<u64>());
/// 1 回の読み込みで許される最大セクタ数を取得する ioctl。
const DKIOCGETMAXBLOCKCOUNTREAD: u64 = ior(b'd', 64, size_of::<u64>());

/// macOS の raw デバイスをバックエンドにした [`Device`]。
///
/// 読み取り専用。書き込み経路は持たない(PLAN.md 6章 1項)。
#[derive(Debug)]
pub struct MacDevice {
    inner: Mutex<Inner>,
    path: PathBuf,
    len: u64,
    info: DeviceInfo,
}

#[derive(Debug)]
struct Inner {
    file: File,
    bounce: AlignedBuf,
}

impl MacDevice {
    /// デバイスを読み取り専用で開く。
    ///
    /// `id` は `disk4` / `/dev/disk4` / `/dev/rdisk4` のいずれの形でもよい。
    /// 常に raw デバイス(`/dev/rdiskN`)を開く。
    pub fn open(id: &str) -> Result<Self> {
        let path = raw_device_path(id);
        let file = open_read_only(&path)?;

        let block_size = ioctl_u32(&file, DKIOCGETBLOCKSIZE).unwrap_or(FALLBACK_BLOCK_SIZE);
        let block_size = if block_size == 0 {
            FALLBACK_BLOCK_SIZE
        } else {
            block_size
        };
        let blocks = ioctl_u64(&file, DKIOCGETBLOCKCOUNT).unwrap_or(0);
        let len = blocks.saturating_mul(u64::from(block_size));
        let bounce_size = max_read_size(&file, block_size);

        let bsd = bsd_name(id);
        let mut info = describe_disk(&bsd, &system_disk_ids());
        info.size_bytes = len;
        info.block_size = block_size;
        info.kind = DeviceKind::PhysicalDisk;

        Ok(Self {
            inner: Mutex::new(Inner {
                file,
                bounce: AlignedBuf::new(bounce_size, BOUNCE_ALIGN),
            }),
            path,
            len,
            info,
        })
    }

    /// 開いている raw デバイスのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Device for MacDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut inner = self.inner.lock().expect("MacDevice poisoned");
        let Inner { file, bounce } = &mut *inner;
        let block_size = self.info.block_size;
        read_via_bounce(
            offset,
            buf,
            self.len,
            block_size,
            bounce.as_mut_slice(),
            |aligned, dst| loop {
                match file.read_at(dst, aligned) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(classify_read_error(aligned, dst.len(), e)),
                }
            },
        )
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

    fn reopen(&self) -> Result<bool> {
        let mut inner = self.inner.lock().expect("MacDevice poisoned");
        inner.file = open_read_only(&self.path)?;
        Ok(true)
    }
}

/// リムーバブル/固定を問わず、物理ディスク単位でデバイスを列挙する。
///
/// 起動ディスクも一覧には出すが [`DeviceInfo::is_system_disk`] を立てて
/// 選択できないようにする(PLAN.md 5.1)。
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let system = system_disk_ids();
    let disks = match whole_disks_from_diskutil() {
        Some(disks) if !disks.is_empty() => disks,
        _ => whole_disks_from_dev()?,
    };

    let mut out = Vec::with_capacity(disks.len());
    for bsd in disks {
        out.push(describe_disk(&bsd, &system));
    }
    out.sort_by_key(|d| natural_disk_order(&d.id));
    Ok(out)
}

/// デバイスを開く。`id` は [`list_devices`] が返す `DeviceInfo::id`。
pub fn open_device(id: &str) -> Result<Box<dyn Device>> {
    Ok(Box::new(MacDevice::open(id)?))
}

/// パスが載っているディスクの BSD 名(`disk4` など)。
///
/// 復元先が復旧元と同じデバイスでないかの判定に使う(PLAN.md 6章 2項)。
pub fn disk_id_for_path(path: &Path) -> Option<String> {
    let mnt = statfs_mntfromname(path)?;
    let base = whole_disk_of(&bsd_name(&mnt));
    // APFS 合成ディスクなら、その裏にある物理ディスクを答える。
    let physical = physical_stores_of(&base).into_iter().next().unwrap_or(base);
    Some(format!("/dev/{physical}"))
}

/// ディスクをアンマウントする(`diskutil unmountDisk`)。
///
/// マウント中でも raw デバイスは読めるが、OS のキャッシュや Spotlight の
/// アクセスを止めたい場合に使う。**呼び出す前に必ずユーザーへ確認すること**
/// (PLAN.md 5.1)。
pub fn unmount_device(id: &str) -> Result<()> {
    let bsd = bsd_name(id);
    let out = Command::new("diskutil")
        .args(["unmountDisk", &bsd])
        .output()
        .map_err(|e| DeviceError::Query {
            what: format!("diskutil unmountDisk {bsd}"),
            source: e,
        })?;
    if out.status.success() {
        return Ok(());
    }
    Err(DeviceError::Query {
        what: format!("diskutil unmountDisk {bsd}"),
        source: io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    })
}

// ---- 内部ヘルパ ----------------------------------------------------------

fn open_read_only(path: &Path) -> Result<File> {
    File::open(path).map_err(|e| match e.raw_os_error() {
        Some(libc::EBUSY) => DeviceError::Busy {
            path: path.to_path_buf(),
        },
        Some(libc::EACCES) | Some(libc::EPERM) => DeviceError::PermissionDenied {
            path: path.to_path_buf(),
            source: e,
        },
        Some(libc::ENOENT) => DeviceError::NotFound(path.display().to_string()),
        _ => DeviceError::Io {
            offset: 0,
            len: 0,
            source: e,
        },
    })
}

/// 1 回の読み込みの上限を決める。
///
/// raw デバイスには 1 回の転送サイズの上限があり、超えると `EINVAL` になる。
/// デバイスに聞いて、上限を超えない範囲で一番大きい値(最大 [`BOUNCE_SIZE`])を使う。
/// 聞けなければ既定値のままにする(その場合でも、エラーが続けばイメージング側が
/// 読み込み単位を縮小するので詰まりはしない)。
fn max_read_size(file: &File, block_size: u32) -> usize {
    let by_bytes = ioctl_u64(file, DKIOCGETMAXBYTECOUNTREAD).filter(|v| *v > 0);
    let by_blocks = ioctl_u64(file, DKIOCGETMAXBLOCKCOUNTREAD)
        .filter(|v| *v > 0)
        .map(|blocks| blocks.saturating_mul(u64::from(block_size)));

    let limit = match (by_bytes, by_blocks) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return BOUNCE_SIZE,
    };
    // セクタ境界に切り下げてから使う。最低でも 1 セクタは読めるようにする。
    let limit = (limit / u64::from(block_size)).max(1) * u64::from(block_size);
    limit.min(BOUNCE_SIZE as u64) as usize
}

fn ioctl_u32(file: &File, request: u64) -> Option<u32> {
    let mut value: u32 = 0;
    // SAFETY: request は u32 を書き戻す _IOR なので、渡すポインタの型と一致する。
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), request, &mut value) };
    (rc == 0).then_some(value)
}

fn ioctl_u64(file: &File, request: u64) -> Option<u64> {
    let mut value: u64 = 0;
    // SAFETY: request は u64 を書き戻す _IOR なので、渡すポインタの型と一致する。
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), request, &mut value) };
    (rc == 0).then_some(value)
}

/// `disk4` / `/dev/disk4` / `/dev/rdisk4s1` → `disk4s1` のように BSD 名だけ取り出す。
fn bsd_name(id: &str) -> String {
    let name = id.rsplit('/').next().unwrap_or(id);
    name.strip_prefix('r').unwrap_or(name).to_string()
}

/// `disk4s1` → `disk4`。パーティション番号を落として物理ディスクの名前にする。
fn whole_disk_of(bsd: &str) -> String {
    let Some(rest) = bsd.strip_prefix("disk") else {
        return bsd.to_string();
    };
    let num: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if num.is_empty() {
        bsd.to_string()
    } else {
        format!("disk{num}")
    }
}

fn raw_device_path(id: &str) -> PathBuf {
    PathBuf::from(format!("/dev/r{}", bsd_name(id)))
}

/// `disk10` が `disk2` より後に来るように数値で比較する。
fn natural_disk_order(id: &str) -> (String, u64) {
    let bsd = bsd_name(id);
    let digits: String = bsd.chars().filter(|c| c.is_ascii_digit()).collect();
    let prefix: String = bsd.chars().take_while(|c| !c.is_ascii_digit()).collect();
    (prefix, digits.parse().unwrap_or(0))
}

fn diskutil_plist(args: &[&str]) -> Option<String> {
    let out = Command::new("diskutil").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn whole_disks_from_diskutil() -> Option<Vec<String>> {
    let xml = diskutil_plist(&["list", "-plist"])?;
    Some(plist::string_array(&xml, "WholeDisks"))
}

fn whole_disks_from_dev() -> Result<Vec<String>> {
    let mut out = Vec::new();
    let dir = std::fs::read_dir("/dev").map_err(|e| DeviceError::Query {
        what: "/dev の走査".to_string(),
        source: e,
    })?;
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // rdiskN(パーティションの rdiskNsM は除く)だけを拾う。
        let Some(num) = name.strip_prefix("rdisk") else {
            continue;
        };
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            out.push(name.trim_start_matches('r').to_string());
        }
    }
    Ok(out)
}

/// `diskutil info -plist` の結果から [`DeviceInfo`] を組み立てる。
///
/// `diskutil` が使えない場合も、識別子だけ埋めた最低限の情報を返す。
fn describe_disk(bsd: &str, system: &HashSet<String>) -> DeviceInfo {
    let id = format!("/dev/{bsd}");
    let xml = diskutil_plist(&["info", "-plist", bsd]);

    let (name, size, block_size, removable, serial) = match &xml {
        Some(xml) => {
            let name = plist::string(xml, "MediaName")
                .or_else(|| plist::string(xml, "IORegistryEntryName"))
                .map(str::to_string)
                .filter(|s| !s.is_empty() && s != "Uninitialized")
                .unwrap_or_else(|| bsd.to_string());
            let size = plist::integer(xml, "TotalSize")
                .or_else(|| plist::integer(xml, "Size"))
                .unwrap_or(0);
            let block_size = plist::integer(xml, "DeviceBlockSize")
                .unwrap_or(u64::from(FALLBACK_BLOCK_SIZE)) as u32;
            // 「取り外せる」判定は複数のキーのどれかが立っていればよい。
            let removable = plist::boolean(xml, "RemovableMediaOrExternalDevice")
                .or_else(|| plist::boolean(xml, "Removable"))
                .or_else(|| plist::boolean(xml, "RemovableMedia"))
                .or_else(|| plist::boolean(xml, "Ejectable"))
                .or_else(|| plist::boolean(xml, "Internal").map(|internal| !internal))
                .unwrap_or(false);
            // diskutil はシリアル番号を出さないので、代わりにディスク UUID を識別子にする。
            let serial = plist::string(xml, "DiskUUID")
                .map(str::to_string)
                .filter(|s| !s.is_empty());
            (name, size, block_size, removable, serial)
        }
        None => (bsd.to_string(), 0, FALLBACK_BLOCK_SIZE, false, None),
    };

    let mut info = DeviceInfo::new(id, name, DeviceKind::PhysicalDisk, size, block_size.max(1));
    info.removable = removable;
    info.serial = serial;
    info.is_system_disk = system.contains(bsd);
    info
}

/// 起動ディスクとして扱う BSD 名の集合。
///
/// `/` が載っているディスクと、それが APFS 合成ディスクなら物理ストアも含める。
fn system_disk_ids() -> HashSet<String> {
    let mut set = HashSet::new();
    let Some(mnt) = statfs_mntfromname(Path::new("/")) else {
        return set;
    };
    let root = whole_disk_of(&bsd_name(&mnt));
    for store in physical_stores_of(&root) {
        set.insert(store);
    }
    set.insert(root);
    set
}

/// APFS 合成ディスクの物理ストア(`disk0s2` → `disk0`)。
fn physical_stores_of(bsd: &str) -> Vec<String> {
    let Some(xml) = diskutil_plist(&["info", "-plist", bsd]) else {
        return Vec::new();
    };
    plist::all_strings(&xml, "APFSPhysicalStore")
        .into_iter()
        .map(|s| whole_disk_of(&bsd_name(s)))
        .collect()
}

/// `statfs(2)` の `f_mntfromname`(例: `/dev/disk3s1s1`)。
fn statfs_mntfromname(path: &Path) -> Option<String> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statfs は書き込み先の構造体をゼロ初期化しておけばよい。
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: c_path は NUL 終端、buf は有効な statfs。
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return None;
    }
    // SAFETY: f_mntfromname は NUL 終端の C 文字列。
    let name = unsafe { CStr::from_ptr(buf.f_mntfromname.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_device_names() {
        assert_eq!(bsd_name("disk4"), "disk4");
        assert_eq!(bsd_name("/dev/disk4"), "disk4");
        assert_eq!(bsd_name("/dev/rdisk4"), "disk4");
        assert_eq!(bsd_name("/dev/rdisk4s1"), "disk4s1");
        assert_eq!(raw_device_path("/dev/disk4"), PathBuf::from("/dev/rdisk4"));
    }

    #[test]
    fn finds_whole_disk_of_partition() {
        assert_eq!(whole_disk_of("disk4s1"), "disk4");
        assert_eq!(whole_disk_of("disk3s1s1"), "disk3");
        assert_eq!(whole_disk_of("disk0"), "disk0");
    }

    #[test]
    fn ioctl_codes_match_xnu() {
        // xnu の sys/disk.h に載っている値と一致すること。
        assert_eq!(DKIOCGETBLOCKSIZE, 0x4004_6418);
        assert_eq!(DKIOCGETBLOCKCOUNT, 0x4008_6419);
        assert_eq!(DKIOCGETMAXBLOCKCOUNTREAD, 0x4008_6440);
        assert_eq!(DKIOCGETMAXBYTECOUNTREAD, 0x4008_6446);
    }

    #[test]
    fn read_size_falls_back_when_the_device_cannot_answer() {
        // 通常のファイルは disk 系 ioctl に答えないので、既定値になる。
        let file = File::open("/dev/null").unwrap();
        assert_eq!(max_read_size(&file, 512), BOUNCE_SIZE);
    }

    #[test]
    fn sorts_disks_numerically() {
        let mut ids = ["/dev/disk10", "/dev/disk2", "/dev/disk1"];
        ids.sort_by_key(|id| natural_disk_order(id));
        assert_eq!(ids, ["/dev/disk1", "/dev/disk2", "/dev/disk10"]);
    }

    #[test]
    fn root_is_a_system_disk() {
        // 実機でしか意味がないが、少なくとも panic しないこと。
        let ids = system_disk_ids();
        if let Some(mnt) = statfs_mntfromname(Path::new("/")) {
            assert!(ids.contains(&whole_disk_of(&bsd_name(&mnt))));
        }
    }
}
