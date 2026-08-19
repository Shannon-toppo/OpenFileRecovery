//! 不良セクタを避けてバッファを埋める共通処理。
//!
//! 壊れかけメディアを直接カービングすると、64KiB のまとめ読みが不良セクタ 1 個で
//! 丸ごと失敗する。それでは不良セクタの周り 64KiB が読めなくなってしまうので、
//! 失敗したら読み込み単位を縮めて取り直し、成功したらまた広げる
//! (PLAN.md 5.2 の「読み込み単位はデバイスの応答を見て動的に縮小する」と同じ考え方)。

use ofr_device::Device;
use tracing::trace;

/// 通常の読み込み単位。
const MAX_STEP: usize = 64 * 1024;
/// 縮小の下限。これで読めなければその領域は諦める。
const MIN_STEP: usize = 512;

/// 埋めた結果。
pub(crate) struct Fill {
    /// バッファの先頭から何バイト埋まったか(ゼロで埋めた分を含む)。
    pub filled: usize,
    /// 読み込みに失敗した回数。
    pub errors: u64,
    /// 読めずにゼロで埋めたバイト数。`skip_bad` が偽なら常に 0。
    pub bad: usize,
}

/// `offset` から `buf` を埋める。
///
/// `skip_bad` が真なら、読めない領域をゼロのまま読み飛ばして最後まで進む
/// (走査用)。偽なら読めない所で止まり、そこまでの長さを返す(解析用)。
pub(crate) fn fill(device: &dyn Device, offset: u64, buf: &mut [u8], skip_bad: bool) -> Fill {
    let mut done = 0usize;
    let mut errors = 0u64;
    let mut bad = 0usize;
    let mut step = MAX_STEP;

    while done < buf.len() {
        let n = (buf.len() - done).min(step);
        let at = offset + done as u64;
        match device.read_at(at, &mut buf[done..done + n]) {
            // デバイス末尾。
            Ok(0) => break,
            Ok(got) => {
                done += got.min(n);
                // 調子が良ければ読み込み単位を戻していく。
                step = (step * 2).min(MAX_STEP);
            }
            Err(e) => {
                errors += 1;
                trace!(offset = at, len = n, error = %e, "カービング中の読み込みエラー");
                if step > MIN_STEP {
                    // 不良セクタの手前までを救うため、細かくして読み直す。
                    step = (step / 8).max(MIN_STEP);
                    continue;
                }
                if !skip_bad {
                    break;
                }
                buf[done..done + n].fill(0);
                done += n;
                bad += n;
            }
        }
    }

    Fill {
        filled: done,
        errors,
        bad,
    }
}
