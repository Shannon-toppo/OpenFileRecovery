//! バリデータ用の読み出しヘルパ。
//!
//! バリデータはヘッダを 4 バイトずつ辿ったり、数 MiB 先の終端マーカーを探したりする。
//! そのたびにデバイスを叩くと壊れかけメディアには酷なので、ここでスライド窓に
//! キャッシュしてから配る。
//!
//! # 契約
//!
//! [`Reader::view`] は**要求より短いスライスを返すことがある**。不良セクタに
//! ぶつかった場合とデバイス末尾の場合で、どちらも「そこから先は読めない」を意味する。
//! バリデータは長さを必ず確認し、足りなければその候補を諦めること
//! (PLAN.md 6章 5項: 不正なバイト列で panic しない)。

use memchr::memmem;
use ofr_device::Device;

use crate::fill;

/// スライド窓つきの読み出し口。
pub struct Reader<'a> {
    device: &'a dyn Device,
    len: u64,
    buf: Vec<u8>,
    /// `buf[0]` が対応するデバイス上の位置。
    start: u64,
    /// `buf` のうち実際に読めているバイト数。
    filled: usize,
    capacity: usize,
    read_errors: u64,
}

impl<'a> Reader<'a> {
    /// 窓の大きさを指定して作る。
    pub fn new(device: &'a dyn Device, capacity: usize) -> Self {
        let capacity = capacity.max(64 * 1024);
        Self {
            device,
            len: device.len(),
            buf: Vec::new(),
            start: 0,
            filled: 0,
            capacity,
            read_errors: 0,
        }
    }

    /// 読み込みに失敗した回数。
    pub fn read_errors(&self) -> u64 {
        self.read_errors
    }

    /// `offset` から最大 `want` バイトを見る。
    ///
    /// 返り値は要求より短いことがある(不良セクタ / デバイス末尾 / 窓の上限)。
    pub fn view(&mut self, offset: u64, want: usize) -> &[u8] {
        if want == 0 || offset >= self.len {
            return &[];
        }
        let want = want.min(self.capacity);
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

    /// ちょうど `N` バイト読む。足りなければ `None`。
    pub fn array<const N: usize>(&mut self, offset: u64) -> Option<[u8; N]> {
        let v = self.view(offset, N);
        if v.len() < N {
            return None;
        }
        let mut out = [0u8; N];
        out.copy_from_slice(&v[..N]);
        Some(out)
    }

    /// 1 バイト読む。
    pub fn u8(&mut self, offset: u64) -> Option<u8> {
        self.array::<1>(offset).map(|a| a[0])
    }

    /// ビッグエンディアン u16。
    pub fn u16be(&mut self, offset: u64) -> Option<u16> {
        self.array::<2>(offset).map(u16::from_be_bytes)
    }

    /// ビッグエンディアン u32。
    pub fn u32be(&mut self, offset: u64) -> Option<u32> {
        self.array::<4>(offset).map(u32::from_be_bytes)
    }

    /// ビッグエンディアン u64。
    pub fn u64be(&mut self, offset: u64) -> Option<u64> {
        self.array::<8>(offset).map(u64::from_be_bytes)
    }

    /// リトルエンディアン u16。
    pub fn u16le(&mut self, offset: u64) -> Option<u16> {
        self.array::<2>(offset).map(u16::from_le_bytes)
    }

    /// リトルエンディアン u32。
    pub fn u32le(&mut self, offset: u64) -> Option<u32> {
        self.array::<4>(offset).map(u32::from_le_bytes)
    }

    /// `offset` から始まるバイト列が `expect` と一致するか。
    pub fn matches(&mut self, offset: u64, expect: &[u8]) -> bool {
        let v = self.view(offset, expect.len());
        v.len() == expect.len() && v == expect
    }

    /// `from` から `until` の手前までで `needle` を探す。
    ///
    /// `needle` 全体が `until` の手前に収まっているものだけを見つける。
    pub fn find(&mut self, needle: &[u8], from: u64, until: u64) -> Option<u64> {
        if needle.is_empty() || from >= until {
            return None;
        }
        let until = until.min(self.len);
        let mut pos = from;
        while pos < until {
            let want = self.capacity.min((until - pos) as usize);
            let view = self.view(pos, want);
            if view.len() < needle.len() {
                return None;
            }
            if let Some(i) = memmem::find(view, needle) {
                return Some(pos + i as u64);
            }
            // 窓の継ぎ目にまたがるヒットを落とさないよう needle-1 バイト戻す。
            pos += (view.len() - needle.len() + 1) as u64;
        }
        None
    }

    /// `from` から `until` の手前までで 1 バイトを探す(SIMD)。
    pub fn find_byte(&mut self, byte: u8, from: u64, until: u64) -> Option<u64> {
        if from >= until {
            return None;
        }
        let until = until.min(self.len);
        let mut pos = from;
        while pos < until {
            let want = self.capacity.min((until - pos) as usize);
            let view = self.view(pos, want);
            if view.is_empty() {
                return None;
            }
            if let Some(i) = memchr::memchr(byte, view) {
                return Some(pos + i as u64);
            }
            pos += view.len() as u64;
        }
        None
    }

    fn refill(&mut self, offset: u64) {
        if self.buf.len() < self.capacity {
            self.buf.resize(self.capacity, 0);
        }
        self.start = offset;
        let max = ((self.len - offset).min(self.capacity as u64)) as usize;
        // 読めない所で止める。呼び出し側は短いスライスを見てその候補を諦める。
        let result = fill::fill(self.device, offset, &mut self.buf[..max], false);
        self.filled = result.filled;
        self.read_errors += result.errors;
    }
}

#[cfg(test)]
mod tests {
    use ofr_device::MockDevice;

    use super::*;

    #[test]
    fn serves_views_across_window_refills() {
        let dev = MockDevice::patterned(300_000);
        let mut r = Reader::new(&dev, 64 * 1024);

        for offset in [0u64, 1, 70_000, 299_990] {
            let v = r.view(offset, 8);
            let expect: Vec<u8> = (0..8)
                .map(|i| MockDevice::pattern_byte(offset + i))
                .take((300_000 - offset).min(8) as usize)
                .collect();
            assert_eq!(v, &expect[..], "offset {offset}");
        }
    }

    #[test]
    fn clamps_at_the_end_of_the_device() {
        let dev = MockDevice::patterned(100);
        let mut r = Reader::new(&dev, 64 * 1024);
        assert_eq!(r.view(96, 8).len(), 4);
        assert_eq!(r.view(100, 8).len(), 0);
        assert_eq!(r.view(1000, 8).len(), 0);
        assert_eq!(r.array::<8>(96), None);
    }

    #[test]
    fn stops_at_bad_sectors_instead_of_erroring() {
        // 不良域はセクタ境界に置く。読み込みは 512 バイト単位まで縮めて粘るので、
        // 不良セクタの直前までは読める。
        let dev = MockDevice::builder(200_000)
            .pattern()
            .bad_range(100_352, 4096)
            .build();
        let mut r = Reader::new(&dev, 128 * 1024);

        // 不良域の手前までは読める。
        assert_eq!(r.view(98_816, 1536).len(), 1536);
        // 不良域にかかると短くなる。
        assert!(r.view(98_816, 4000).len() < 4000);
        // 不良域そのものは空。
        assert!(r.view(100_352, 512).is_empty());
        assert!(r.read_errors() > 0);
        // 不良域の先はまた読める。
        assert_eq!(r.view(104_448, 1000).len(), 1000);
    }

    #[test]
    fn finds_needles_across_window_boundaries() {
        let mut data = vec![0u8; 200_000];
        data[130_000..130_005].copy_from_slice(b"%%EOF");
        // 窓の継ぎ目 (64KiB) をまたぐ位置にも置く。
        data[65_534..65_539].copy_from_slice(b"%%EOF");
        let dev = MockDevice::builder(data.len() as u64).data(data).build();
        let mut r = Reader::new(&dev, 64 * 1024);

        assert_eq!(r.find(b"%%EOF", 0, 200_000), Some(65_534));
        assert_eq!(r.find(b"%%EOF", 65_535, 200_000), Some(130_000));
        assert_eq!(r.find(b"%%EOF", 0, 65_538), None);
        assert_eq!(r.find(b"%%EOF", 130_001, 200_000), None);
        assert_eq!(r.find_byte(b'%', 0, 200_000), Some(65_534));
    }
}
