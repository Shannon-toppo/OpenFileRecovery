//! セクタ整列のためのヘルパ。
//!
//! Windows の `FILE_FLAG_NO_BUFFERING` は読み込みをセクタ境界に整列させる必要がある。
//! 整列はデバイス実装側で吸収し、上位には任意オフセットの read を見せる(PLAN.md 5.1)。

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
}
