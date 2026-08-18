//! デバイスの一部を窓単位でキャッシュして読む。
//!
//! FAT 表やアロケーションビットマップは「4 バイトずつあちこち読む」使い方に
//! なるので、そのたびにデバイスを叩くと遅い。一定サイズの窓をいくつか保持して、
//! 同じ窓への読み込みを 1 回で済ませる。
//!
//! 読み込みに失敗した窓はゼロ埋めして返す。壊れかけメディアでは FAT の一部が
//! 読めないことがあるが、そこで解析全体を止めない(PLAN.md 5.3)。

use std::sync::Mutex;

use ofr_device::Device;

/// 既定の窓サイズ。
pub const DEFAULT_WINDOW: u64 = 64 << 10;
/// 保持する窓の数。
const MAX_WINDOWS: usize = 8;

struct Window {
    start: u64,
    data: Vec<u8>,
    /// 読み込みに失敗した(中身はゼロ)。
    failed: bool,
}

/// デバイス上の連続領域を窓単位で読むキャッシュ。
pub struct WindowCache<'a> {
    device: &'a dyn Device,
    base: u64,
    len: u64,
    window: u64,
    windows: Mutex<Vec<Window>>,
    /// 読み込みに失敗した窓の数。
    failures: Mutex<u32>,
}

impl<'a> WindowCache<'a> {
    /// `base` から `len` バイトの領域を対象にする。
    pub fn new(device: &'a dyn Device, base: u64, len: u64) -> Self {
        Self::with_window(device, base, len, DEFAULT_WINDOW)
    }

    /// 窓サイズを指定して作る。
    pub fn with_window(device: &'a dyn Device, base: u64, len: u64, window: u64) -> Self {
        Self {
            device,
            base,
            len,
            window: window.max(512),
            windows: Mutex::new(Vec::new()),
            failures: Mutex::new(0),
        }
    }

    /// 対象領域の長さ。
    pub fn len(&self) -> u64 {
        self.len
    }

    /// 長さが 0 か。
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 読み込みに失敗した窓の数。
    pub fn failures(&self) -> u32 {
        *self.failures.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 領域内オフセットから u32(LE)を読む。範囲外なら `None`。
    pub fn u32_at(&self, offset: u64) -> Option<u32> {
        let mut buf = [0u8; 4];
        self.copy_into(offset, &mut buf)
            .then(|| u32::from_le_bytes(buf))
    }

    /// 領域内オフセットから 1 バイト読む。範囲外なら `None`。
    pub fn u8_at(&self, offset: u64) -> Option<u8> {
        let mut buf = [0u8; 1];
        self.copy_into(offset, &mut buf).then_some(buf[0])
    }

    /// 領域内オフセットから `buf` を埋める。範囲外を含むなら `false`。
    ///
    /// 窓をまたぐ読み込みにも対応する。
    pub fn copy_into(&self, offset: u64, buf: &mut [u8]) -> bool {
        match offset.checked_add(buf.len() as u64) {
            Some(end) if end <= self.len => {}
            _ => return false,
        }

        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let window_start = pos / self.window * self.window;
            let in_window = (pos - window_start) as usize;
            let copied = self.with_window_data(window_start, |data| {
                let take = (buf.len() - done).min(data.len().saturating_sub(in_window));
                if take > 0 {
                    buf[done..done + take].copy_from_slice(&data[in_window..in_window + take]);
                }
                take
            });
            if copied == 0 {
                return false;
            }
            done += copied;
        }
        true
    }

    fn with_window_data<T>(&self, start: u64, f: impl FnOnce(&[u8]) -> T) -> T {
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(index) = windows.iter().position(|w| w.start == start) {
            // 直近に使ったものを先頭へ寄せる(素朴な LRU)。
            let window = windows.remove(index);
            let result = f(&window.data);
            windows.insert(0, window);
            return result;
        }

        let size = self.window.min(self.len.saturating_sub(start)) as usize;
        let mut data = vec![0u8; size];
        let mut failed = false;
        if size > 0
            && let Err(e) = self.device.read_exact_at(self.base + start, &mut data)
        {
            tracing::debug!("キャッシュ窓 {start} の読み込みに失敗: {e}");
            data.iter_mut().for_each(|b| *b = 0);
            failed = true;
            *self.failures.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        }

        let window = Window {
            start,
            data,
            failed,
        };
        let result = f(&window.data);
        windows.insert(0, window);
        windows.truncate(MAX_WINDOWS);
        result
    }

    /// その位置を含む窓の読み込みが失敗していたか。
    pub fn window_failed(&self, offset: u64) -> bool {
        let start = offset / self.window * self.window;
        let windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        windows
            .iter()
            .find(|w| w.start == start)
            .is_some_and(|w| w.failed)
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;

    #[test]
    fn reads_across_window_boundaries() {
        let device = MockDevice::patterned(8192);
        let cache = WindowCache::with_window(&device, 512, 4096, 1024);

        let mut buf = [0u8; 16];
        assert!(cache.copy_into(1020, &mut buf));
        for (i, b) in buf.iter().enumerate() {
            assert_eq!(*b, MockDevice::pattern_byte(512 + 1020 + i as u64));
        }

        assert_eq!(
            cache.u32_at(0),
            Some(u32::from_le_bytes([
                MockDevice::pattern_byte(512),
                MockDevice::pattern_byte(513),
                MockDevice::pattern_byte(514),
                MockDevice::pattern_byte(515),
            ]))
        );
    }

    #[test]
    fn refuses_reads_past_the_region() {
        let device = MockDevice::patterned(8192);
        let cache = WindowCache::new(&device, 0, 1024);
        let mut buf = [0u8; 8];
        assert!(!cache.copy_into(1020, &mut buf));
        assert_eq!(cache.u32_at(1024), None);
    }

    #[test]
    fn unreadable_windows_become_zeros_instead_of_errors() {
        let device = MockDevice::builder(8192)
            .pattern()
            .bad_range(0, 8192)
            .build();
        let cache = WindowCache::with_window(&device, 0, 4096, 1024);
        assert_eq!(cache.u32_at(0), Some(0));
        assert!(cache.window_failed(0));
        assert_eq!(cache.failures(), 1);
    }
}
