//! セクタ整列のためのヘルパ。
//!
//! Windows の `FILE_FLAG_NO_BUFFERING` は読み込みをセクタ境界に整列させる必要がある。
//! 整列はデバイス実装側で吸収し、上位には任意オフセットの read を見せる(PLAN.md 5.1)。

use crate::device::clamp_read;
use crate::error::Result;

/// `value` を `block` の倍数に切り下げる。`block` は 0 でないこと。
pub fn align_down(value: u64, block: u32) -> u64 {
    let block = block as u64;
    value - (value % block)
}

/// `value` を `block` の倍数に切り上げる。飽和加算なのでオーバーフローしない。
pub fn align_up(value: u64, block: u32) -> u64 {
    let block = block as u64;
    let rem = value % block;
    if rem == 0 {
        value
    } else {
        value.saturating_add(block - rem)
    }
}

/// `value` が `block` の倍数か。
pub fn is_aligned(value: u64, block: u32) -> bool {
    value % block as u64 == 0
}

/// 整列が必要なバックエンド用の、バウンスバッファ経由の読み込み。
///
/// `raw_read` にはブロック境界に整列したオフセットと、ブロックサイズの倍数の長さの
/// バッファだけを渡す。上位から見た任意オフセット・任意長の読み込みはここで吸収する。
///
/// `bounce` の長さはブロックサイズの倍数であること。
/// `raw_read` が返すエラーはそのまま伝播するので、エラー内のオフセットは
/// 呼び出し側が要求したものではなく整列後のものになる。
pub(crate) fn read_via_bounce<F>(
    offset: u64,
    buf: &mut [u8],
    device_len: u64,
    block_size: u32,
    bounce: &mut [u8],
    mut raw_read: F,
) -> Result<usize>
where
    F: FnMut(u64, &mut [u8]) -> Result<usize>,
{
    debug_assert!(is_aligned(bounce.len() as u64, block_size));
    let Some(want) = clamp_read(offset, buf.len(), device_len) else {
        return Ok(0);
    };
    // デバイス末尾を越える読み込みは生デバイスでは失敗するので、ここで止める。
    let device_end = align_up(device_len, block_size);

    let mut copied = 0usize;
    while copied < want {
        let cur = offset + copied as u64;
        let aligned = align_down(cur, block_size);
        let skew = (cur - aligned) as usize;
        let remaining = want - copied;

        let need = align_up((skew + remaining) as u64, block_size);
        let chunk = need.min(bounce.len() as u64).min(device_end - aligned) as usize;
        if chunk == 0 {
            break;
        }

        let got = raw_read(aligned, &mut bounce[..chunk])?;
        if got <= skew {
            break; // 予期しない EOF。読めた分だけ返す。
        }
        let take = (got - skew).min(remaining);
        buf[copied..copied + take].copy_from_slice(&bounce[skew..skew + take]);
        copied += take;

        if got < chunk {
            break; // 短い読み込みは末尾に達したということ。
        }
    }
    Ok(copied)
}

/// ブロック境界に整列したバッファ。
///
/// Windows の `FILE_FLAG_NO_BUFFERING` はバッファのアドレスもセクタ境界に
/// 整列していることを要求する。余分に確保して内側の整列した範囲だけを使う
/// ことで、`unsafe` なアロケータ呼び出しを避けている。
#[derive(Debug)]
pub(crate) struct AlignedBuf {
    raw: Vec<u8>,
    offset: usize,
    len: usize,
}

impl AlignedBuf {
    /// `len` バイトを `align` 境界に整列させて確保する。
    pub(crate) fn new(len: usize, align: usize) -> Self {
        assert!(align.is_power_of_two(), "align は 2 の冪");
        let raw = vec![0u8; len + align];
        // アドレスを見るだけ。参照外しはしないので安全なコードで書ける。
        let addr = raw.as_ptr() as usize;
        let offset = (align - (addr % align)) % align;
        Self { raw, offset, len }
    }

    /// 整列した領域。
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        let (start, len) = (self.offset, self.len);
        &mut self.raw[start..start + len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_down_and_up() {
        assert_eq!(align_down(0, 512), 0);
        assert_eq!(align_down(511, 512), 0);
        assert_eq!(align_down(512, 512), 512);
        assert_eq!(align_down(1000, 512), 512);

        assert_eq!(align_up(0, 512), 0);
        assert_eq!(align_up(1, 512), 512);
        assert_eq!(align_up(512, 512), 512);
        assert_eq!(align_up(513, 512), 1024);
    }

    #[test]
    fn align_up_saturates_instead_of_overflowing() {
        assert_eq!(align_up(u64::MAX, 512), u64::MAX);
    }

    #[test]
    fn checks_alignment() {
        assert!(is_aligned(0, 512));
        assert!(is_aligned(4096, 512));
        assert!(!is_aligned(4097, 512));
    }

    #[test]
    fn bounce_read_absorbs_unaligned_requests() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let mut bounce = vec![0u8; 1024];
        let mut requested: Vec<(u64, usize)> = Vec::new();

        let mut out = vec![0u8; 700];
        let n = read_via_bounce(
            513,
            &mut out,
            data.len() as u64,
            512,
            &mut bounce,
            |off, dst| {
                requested.push((off, dst.len()));
                let off = off as usize;
                let end = (off + dst.len()).min(data.len());
                dst[..end - off].copy_from_slice(&data[off..end]);
                Ok(end - off)
            },
        )
        .unwrap();

        assert_eq!(n, 700);
        assert_eq!(out, &data[513..1213]);
        // 生の読み込みは常に整列している。
        for (off, len) in requested {
            assert!(is_aligned(off, 512), "offset {off} が未整列");
            assert!(is_aligned(len as u64, 512), "len {len} が未整列");
        }
    }

    #[test]
    fn bounce_read_clamps_at_device_end() {
        let data = vec![7u8; 1000];
        let mut bounce = vec![0u8; 512];
        let mut out = vec![0u8; 512];

        let n = read_via_bounce(900, &mut out, 1000, 512, &mut bounce, |off, dst| {
            let off = off as usize;
            let end = (off + dst.len()).min(data.len());
            dst[..end - off].copy_from_slice(&data[off..end]);
            Ok(end - off)
        })
        .unwrap();

        assert_eq!(n, 100);
        assert!(out[..100].iter().all(|b| *b == 7));
    }

    #[test]
    fn aligned_buf_is_aligned() {
        let mut buf = AlignedBuf::new(4096, 4096);
        assert_eq!(buf.as_mut_slice().len(), 4096);
        let addr = buf.as_mut_slice().as_ptr() as usize;
        assert_eq!(addr % 4096, 0);
    }
}
