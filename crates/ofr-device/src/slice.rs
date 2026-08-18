//! 親デバイスの一部分だけを見せる読み取り専用ビュー。
//!
//! パーティション内のファイルシステムを解析するときに使う。解析側はオフセット 0 から
//! 始まるボリュームとして扱えるので、パーティションの開始位置を意識しなくてよくなる。

use crate::device::{Device, DeviceInfo, clamp_read};
use crate::error::{DeviceError, Result};

/// 親デバイスの `[offset, offset + len)` だけを見せるビュー。
///
/// ```
/// use ofr_device::{Device, MockDevice, SliceDevice};
///
/// let parent = MockDevice::patterned(4096);
/// let view = SliceDevice::new(&parent, 1024, 512).unwrap();
///
/// let mut buf = [0u8; 8];
/// assert_eq!(view.len(), 512);
/// view.read_exact_at(0, &mut buf).unwrap();
/// assert_eq!(buf[0], MockDevice::pattern_byte(1024));
/// ```
#[derive(Debug)]
pub struct SliceDevice<D> {
    inner: D,
    offset: u64,
    len: u64,
    info: DeviceInfo,
}

impl<D: Device> SliceDevice<D> {
    /// 親デバイスの一部を切り出す。範囲が親をはみ出すなら
    /// [`DeviceError::OutOfRange`]。
    pub fn new(inner: D, offset: u64, len: u64) -> Result<Self> {
        let parent_len = inner.len();
        if offset > parent_len || len > parent_len - offset {
            return Err(DeviceError::OutOfRange {
                offset,
                len,
                device_len: parent_len,
            });
        }

        // id は親のまま残す。「復元先が復旧元と同じデバイス」の判定
        // (PLAN.md 6章 2項) は物理デバイス単位で行うため。
        let mut info = inner.info().clone();
        info.display_name = format!("{} +{offset}", info.display_name);
        info.size_bytes = len;

        Ok(Self {
            inner,
            offset,
            len,
            info,
        })
    }

    /// 親デバイス上での開始オフセット。
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// 親デバイス。
    pub fn parent(&self) -> &D {
        &self.inner
    }
}

impl<D: Device> Device for SliceDevice<D> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let Some(want) = clamp_read(offset, buf.len(), self.len) else {
            return Ok(0);
        };
        self.inner.read_at(self.offset + offset, &mut buf[..want])
    }

    fn len(&self) -> u64 {
        self.len
    }

    fn block_size(&self) -> u32 {
        self.inner.block_size()
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn reopen(&self) -> Result<bool> {
        self.inner.reopen()
    }
}
