//! Windows の生デバイスアクセスと列挙。
//!
//! - 読み込みは `\\.\PhysicalDriveN` を `GENERIC_READ` のみ・
//!   `FILE_FLAG_NO_BUFFERING` 付きで開いて行う。非バッファ IO はオフセット・長さ・
//!   バッファアドレスの全てをセクタ境界に整列させる必要があるので、整列は
//!   [`crate::align::read_via_bounce`] と [`crate::align::AlignedBuf`] で吸収する
//!   (PLAN.md 5.1)。
//! - 列挙は `IOCTL_STORAGE_QUERY_PROPERTY` と `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`。
//!   照会はアクセス権 0 でハンドルを開くので管理者権限なしでも動く。
//! - 起動ディスクは Windows ディレクトリのあるボリュームから
//!   `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` で辿って判定する(PLAN.md 6章 3項)。
//!
//! 生デバイスの読み込みには管理者権限が必要。Phase 1 では管理者コマンドプロンプトから
//! CLI を実行する前提とし、GUI の manifest 対応は Phase 6 で行う(PLAN.md 10章)。

// Win32 の FFI を使うのはこのモジュールだけ。
#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::FileExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, GetVolumePathNameW};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_STORAGE_QUERY_PROPERTY,
    PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    StorageDeviceProperty, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

use crate::align::{AlignedBuf, read_via_bounce};
use crate::device::{Device, DeviceInfo, DeviceKind};
use crate::error::{DeviceError, Result, classify_read_error};

/// バウンスバッファの大きさ。1 回の生読み込みの上限でもある。
const BOUNCE_SIZE: usize = 1 << 20;
/// 非バッファ IO 用のアドレス整列。物理セクタサイズより大きく取っておけば安全。
const BOUNCE_ALIGN: usize = 4096;
/// 走査する `\\.\PhysicalDriveN` の上限。
const MAX_PHYSICAL_DRIVES: u32 = 64;
/// セクタサイズが取れなかったときの仮定値。
const FALLBACK_BLOCK_SIZE: u32 = 512;

// windows-sys の型名の揺れを避けるため、単純な定数は自前で持つ。
const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
/// windows-sys が出力していないので自前で持つ(winioctl.h の
/// `CTL_CODE(IOCTL_VOLUME_BASE, 0, METHOD_BUFFERED, FILE_ANY_ACCESS)`)。
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
/// USB バス。リムーバブル判定に使う。
const BUS_TYPE_USB: i32 = 0x07;
/// SD カード。
const BUS_TYPE_SD: i32 = 0x0C;
/// MMC。
const BUS_TYPE_MMC: i32 = 0x0D;

/// Windows の物理ドライブをバックエンドにした [`Device`]。
///
/// 読み取り専用。書き込み経路は持たない(PLAN.md 6章 1項)。
#[derive(Debug)]
pub struct WindowsDevice {
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

impl WindowsDevice {
    /// デバイスを読み取り専用で開く。
    ///
    /// `id` は `\\.\PhysicalDrive2` か、単なるドライブ番号 `2` でもよい。
    pub fn open(id: &str) -> Result<Self> {
        let path = physical_drive_path(id);
        let file = open_for_read(&path)?;

        let system = system_disk_numbers();
        let number = drive_number(id);
        let mut info = match number {
            Some(n) => describe_drive(n, &system).unwrap_or_else(|| fallback_info(&path)),
            None => fallback_info(&path),
        };
        if info.size_bytes == 0 || info.block_size == 0 {
            // 照会に失敗していたら、開いたハンドルから取り直す。
            if let Some((size, block)) = drive_geometry(&file) {
                info.size_bytes = size;
                info.block_size = block;
            }
        }
        let len = info.size_bytes;
        let block_size = if info.block_size == 0 {
            FALLBACK_BLOCK_SIZE
        } else {
            info.block_size
        };
        info.block_size = block_size;

        Ok(Self {
            inner: Mutex::new(Inner {
                file,
                bounce: AlignedBuf::new(BOUNCE_SIZE, BOUNCE_ALIGN),
            }),
            path,
            len,
            info,
        })
    }

    /// 開いているデバイスのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Device for WindowsDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut inner = self.inner.lock().expect("WindowsDevice poisoned");
        let Inner { file, bounce } = &mut *inner;
        let block_size = self.info.block_size;
        read_via_bounce(
            offset,
            buf,
            self.len,
            block_size,
            bounce.as_mut_slice(),
            |aligned, dst| loop {
                match file.seek_read(dst, aligned) {
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
        let mut inner = self.inner.lock().expect("WindowsDevice poisoned");
        inner.file = open_for_read(&self.path)?;
        Ok(true)
    }
}

/// 物理ドライブを列挙する。
///
/// 起動ディスクも一覧には出すが [`DeviceInfo::is_system_disk`] を立てる
/// (PLAN.md 5.1 / 6章 3項)。
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let system = system_disk_numbers();
    let mut out = Vec::new();
    for n in 0..MAX_PHYSICAL_DRIVES {
        if let Some(info) = describe_drive(n, &system) {
            out.push(info);
        }
    }
    Ok(out)
}

/// デバイスを開く。`id` は [`list_devices`] が返す `DeviceInfo::id`。
pub fn open_device(id: &str) -> Result<Box<dyn Device>> {
    Ok(Box::new(WindowsDevice::open(id)?))
}

/// パスが載っている物理ドライブの ID(`\\.\PhysicalDrive2`)。
pub fn disk_id_for_path(path: &Path) -> Option<String> {
    let root = volume_path_name(path)?;
    let numbers = disk_numbers_of_volume(&root);
    numbers.first().map(|n| format!(r"\\.\PhysicalDrive{n}"))
}

/// Windows ではアンマウントせずに生デバイスを読めるので、何もしない。
pub fn unmount_device(_id: &str) -> Result<()> {
    Err(DeviceError::Unsupported(
        "Windows ではアンマウントせずに生デバイスを読める".to_string(),
    ))
}

// ---- 内部ヘルパ ----------------------------------------------------------

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn physical_drive_path(id: &str) -> PathBuf {
    match drive_number(id) {
        Some(n) => PathBuf::from(format!(r"\\.\PhysicalDrive{n}")),
        None => PathBuf::from(id),
    }
}

/// `\\.\PhysicalDrive2` / `PhysicalDrive2` / `2` からドライブ番号を取り出す。
fn drive_number(id: &str) -> Option<u32> {
    let digits: String = id
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// アクセス権 0 で開く照会用ハンドル。管理者権限は不要。
fn open_for_query(path: &str) -> Option<File> {
    open_handle(path, 0, 0).ok()
}

fn open_for_read(path: &Path) -> Result<File> {
    let path_str = path.to_string_lossy().into_owned();
    open_handle(&path_str, GENERIC_READ, FILE_FLAG_NO_BUFFERING).map_err(|e| {
        match e.raw_os_error() {
            Some(ERROR_ACCESS_DENIED) => DeviceError::PermissionDenied {
                path: path.to_path_buf(),
                source: e,
            },
            Some(ERROR_SHARING_VIOLATION) => DeviceError::Busy {
                path: path.to_path_buf(),
            },
            _ if e.kind() == io::ErrorKind::NotFound => DeviceError::NotFound(path_str.clone()),
            _ => DeviceError::Io {
                offset: 0,
                len: 0,
                source: e,
            },
        }
    })
}

fn open_handle(path: &str, access: u32, flags: u32) -> io::Result<File> {
    let wide_path = wide(path);
    // SAFETY: wide_path は NUL 終端の UTF-16。他の引数は Win32 の規定値。
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW が返した有効なハンドルの所有権を File に渡す。
    // 以降のクローズは File の Drop が行う。
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

/// `DeviceIoControl` の薄いラッパ。成功したら書き戻されたバイト数を返す。
fn device_io_control(
    file: &File,
    code: u32,
    input: Option<&[u8]>,
    output: &mut [u8],
) -> Option<usize> {
    let mut returned: u32 = 0;
    let (in_ptr, in_len) = match input {
        Some(buf) => (buf.as_ptr() as *const c_void, buf.len() as u32),
        None => (std::ptr::null(), 0),
    };
    // SAFETY: 入出力バッファはどちらも有効なスライスで、長さも実際の値を渡している。
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as _,
            code,
            in_ptr,
            in_len,
            output.as_mut_ptr() as *mut c_void,
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(returned as usize)
}

/// `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` から (総サイズ, セクタサイズ)。
fn drive_geometry(file: &File) -> Option<(u64, u32)> {
    let mut buf = [0u8; 256];
    device_io_control(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, None, &mut buf)?;
    if size_of::<DISK_GEOMETRY_EX>() > buf.len() {
        return None;
    }
    // SAFETY: DeviceIoControl が DISK_GEOMETRY_EX を書き込んだバッファを読み出す。
    // 構造体はサイズが判明していて、buf は十分な大きさと整列を持つ。
    let geo: DISK_GEOMETRY_EX = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const _) };
    let size = geo.DiskSize as u64;
    let block = geo.Geometry.BytesPerSector;
    Some((
        size,
        if block == 0 {
            FALLBACK_BLOCK_SIZE
        } else {
            block
        },
    ))
}

/// `IOCTL_STORAGE_QUERY_PROPERTY` の結果(製品名, シリアル, リムーバブルか)。
fn storage_descriptor(file: &File) -> Option<(String, Option<String>, bool)> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // SAFETY: STORAGE_PROPERTY_QUERY は POD なので、そのままバイト列として渡せる。
    let query_bytes = unsafe {
        std::slice::from_raw_parts(
            (&query as *const STORAGE_PROPERTY_QUERY) as *const u8,
            size_of::<STORAGE_PROPERTY_QUERY>(),
        )
    };

    let mut buf = vec![0u8; 1024];
    let written = device_io_control(
        file,
        IOCTL_STORAGE_QUERY_PROPERTY,
        Some(query_bytes),
        &mut buf,
    )?;
    if written < size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return None;
    }
    // SAFETY: 上で必要バイト数が書き戻されたことを確認済み。
    let desc: STORAGE_DEVICE_DESCRIPTOR =
        unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const _) };

    let vendor = ansi_at(&buf, desc.VendorIdOffset as usize);
    let product = ansi_at(&buf, desc.ProductIdOffset as usize);
    let serial = ansi_at(&buf, desc.SerialNumberOffset as usize);

    let name = match (vendor, product) {
        (Some(v), Some(p)) => format!("{v} {p}").trim().to_string(),
        (None, Some(p)) => p,
        (Some(v), None) => v,
        (None, None) => String::new(),
    };
    let removable =
        desc.RemovableMedia || matches!(desc.BusType, BUS_TYPE_USB | BUS_TYPE_SD | BUS_TYPE_MMC);
    Some((name, serial, removable))
}

/// 記述子バッファ内のオフセットにある NUL 終端 ANSI 文字列。
fn ansi_at(buf: &[u8], offset: usize) -> Option<String> {
    if offset == 0 || offset >= buf.len() {
        return None;
    }
    let bytes = &buf[offset..];
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let s = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn describe_drive(number: u32, system: &[u32]) -> Option<DeviceInfo> {
    let id = format!(r"\\.\PhysicalDrive{number}");
    let file = open_for_query(&id)?;

    let (size, block) = drive_geometry(&file).unwrap_or((0, FALLBACK_BLOCK_SIZE));
    let (name, serial, removable) = storage_descriptor(&file)
        .unwrap_or_else(|| (format!("PhysicalDrive{number}"), None, false));

    let display = if name.is_empty() {
        format!("PhysicalDrive{number}")
    } else {
        name
    };
    let mut info = DeviceInfo::new(id, display, DeviceKind::PhysicalDisk, size, block);
    info.removable = removable;
    info.serial = serial;
    info.is_system_disk = system.contains(&number);
    Some(info)
}

fn fallback_info(path: &Path) -> DeviceInfo {
    DeviceInfo::new(
        path.to_string_lossy().into_owned(),
        path.to_string_lossy().into_owned(),
        DeviceKind::PhysicalDisk,
        0,
        FALLBACK_BLOCK_SIZE,
    )
}

/// Windows ディレクトリのあるボリュームが載っている物理ドライブ番号。
fn system_disk_numbers() -> Vec<u32> {
    let mut buf = [0u16; 260];
    // SAFETY: buf は呼び出し先が書き込む固定長バッファ。長さも渡している。
    let len = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len > buf.len() {
        return Vec::new();
    }
    let windows_dir = std::ffi::OsString::from_wide(&buf[..len]);
    let Some(root) = volume_path_name(Path::new(&windows_dir)) else {
        return Vec::new();
    };
    disk_numbers_of_volume(&root)
}

/// パスが属するボリュームのマウントルート(`C:\` など)。
fn volume_path_name(path: &Path) -> Option<String> {
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut buf = [0u16; 260];
    // SAFETY: wide_path は NUL 終端、buf は出力用の固定長バッファ。
    let ok = unsafe { GetVolumePathNameW(wide_path.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(
        std::ffi::OsString::from_wide(&buf[..end])
            .to_string_lossy()
            .into_owned(),
    )
}

/// ボリューム(`C:\`)が載っている物理ドライブ番号の一覧。
fn disk_numbers_of_volume(root: &str) -> Vec<u32> {
    let trimmed = root.trim_end_matches('\\');
    if trimmed.is_empty() {
        return Vec::new();
    }
    let Some(file) = open_for_query(&format!(r"\\.\{trimmed}")) else {
        return Vec::new();
    };
    // 複数エクステント(スパンボリューム)に備えて広めに取る。
    let mut buf = vec![0u8; 1024];
    let Some(_) = device_io_control(&file, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, None, &mut buf)
    else {
        return Vec::new();
    };
    // SAFETY: VOLUME_DISK_EXTENTS が書き戻されたバッファを読む。
    let extents: VOLUME_DISK_EXTENTS =
        unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const _) };
    let count = extents.NumberOfDiskExtents.min(8) as usize;

    let mut out = Vec::with_capacity(count);
    let base = std::mem::offset_of!(VOLUME_DISK_EXTENTS, Extents);
    let stride = size_of::<windows_sys::Win32::System::Ioctl::DISK_EXTENT>();
    for i in 0..count {
        let at = base + i * stride;
        if at + stride > buf.len() {
            break;
        }
        // SAFETY: 上で範囲を確認した位置から DISK_EXTENT を 1 個読み出す。
        let extent: windows_sys::Win32::System::Ioctl::DISK_EXTENT =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at) as *const _) };
        out.push(extent.DiskNumber);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_drive_numbers() {
        assert_eq!(drive_number(r"\\.\PhysicalDrive2"), Some(2));
        assert_eq!(drive_number("PhysicalDrive13"), Some(13));
        assert_eq!(drive_number("2"), Some(2));
        assert_eq!(drive_number("disk"), None);
    }

    #[test]
    fn builds_device_paths() {
        assert_eq!(
            physical_drive_path("2"),
            PathBuf::from(r"\\.\PhysicalDrive2")
        );
        assert_eq!(
            physical_drive_path(r"\\.\PhysicalDrive2"),
            PathBuf::from(r"\\.\PhysicalDrive2")
        );
    }

    #[test]
    fn reads_ansi_strings_from_descriptor_buffer() {
        let mut buf = vec![0u8; 32];
        buf[8..14].copy_from_slice(b"SanDsk");
        assert_eq!(ansi_at(&buf, 8).as_deref(), Some("SanDsk"));
        assert_eq!(ansi_at(&buf, 0), None);
        assert_eq!(ansi_at(&buf, 999), None);
    }

    #[test]
    fn enumerates_without_admin_rights() {
        // 列挙は照会ハンドル(アクセス権 0)なので権限なしでも失敗しない。
        let devices = list_devices().expect("列挙は失敗しない");
        for d in &devices {
            assert!(d.block_size >= 1);
        }
    }
}
