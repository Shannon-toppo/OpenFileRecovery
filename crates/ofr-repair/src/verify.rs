//! 修復結果の検証。
//!
//! PLAN.md 5.6 は静止画に「`image` クレートでデコードが通るか、極端な色破綻が
//! ないか(簡易ヒューリスティック)を確認して成否を返す」を求めている。
//! 動画はコンテナ整合性チェックまでで、実視聴での確認は利用者に委ねる
//! (ffmpeg に依存しないため、デコードして確かめる手段がない)。
//!
//! デコード検証は `verify-decode` 機能で切り離してある。GUI もこの機能を
//! 有効にして使う想定で、無効化は「`image` クレートを外して軽くしたい」場合のため。

use crate::format::RepairFormat;
use crate::report::Verification;

/// 検証の結果と、レポートに足したい注記。
pub(crate) struct ImageCheck {
    /// 検証結果。
    pub verification: Verification,
    /// 気付いたこと(ほぼ一色だった、など)。
    pub note: Option<String>,
}

/// 修復した静止画をデコードして確かめる。
#[cfg(feature = "verify-decode")]
pub(crate) fn image_check(format: RepairFormat, data: &[u8], fill: u8) -> ImageCheck {
    use std::io::Cursor;

    let image_format = match format {
        RepairFormat::Jpeg => image::ImageFormat::Jpeg,
        RepairFormat::Png => image::ImageFormat::Png,
        _ => {
            return ImageCheck {
                verification: Verification::Skipped("静止画ではない".to_string()),
                note: None,
            };
        }
    };

    let decoded = image::ImageReader::with_format(Cursor::new(data), image_format).decode();
    match decoded {
        Ok(img) => {
            let (width, height) = (img.width(), img.height());
            let note = flatness_note(&img, fill);
            ImageCheck {
                verification: Verification::Decoded { width, height },
                note,
            }
        }
        Err(e) => ImageCheck {
            verification: Verification::Failed(format!("デコードできない: {e}")),
            note: None,
        },
    }
}

/// デコードは通ったが中身が無い、に近い状態を見つける簡易ヒューリスティック。
///
/// 埋め値だらけの画像は「開けはするが写っていない」ので、成功と言い切ると
/// 利用者を誤解させる。厳密な画質評価はしない(できない)。
#[cfg(feature = "verify-decode")]
fn flatness_note(img: &image::DynamicImage, fill: u8) -> Option<String> {
    use image::GenericImageView;

    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Some("画像の寸法が 0 になっている".to_string());
    }
    // 全画素を見る必要はない。等間隔に最大 4096 点だけ拾う。
    let total = u64::from(w) * u64::from(h);
    let step = (total / 4096).max(1);
    let (mut sampled, mut filled) = (0u64, 0u64);
    let mut i = 0u64;
    while i < total {
        let px = img.get_pixel((i % u64::from(w)) as u32, (i / u64::from(w)) as u32);
        let near_fill = px.0[..3].iter().all(|c| c.abs_diff(fill) <= 2);
        sampled += 1;
        filled += u64::from(near_fill);
        i += step;
    }
    if sampled > 0 && filled * 10 >= sampled * 9 {
        return Some(format!(
            "画像の {}% が埋め値のままで、絵として残っている部分がほとんどない",
            filled * 100 / sampled
        ));
    }
    None
}

/// `verify-decode` を切ったビルド。デコード検証は行わない。
#[cfg(not(feature = "verify-decode"))]
pub(crate) fn image_check(_format: RepairFormat, _data: &[u8], _fill: u8) -> ImageCheck {
    ImageCheck {
        verification: Verification::Skipped(
            "verify-decode 機能が無効なのでデコード検証を行っていない".to_string(),
        ),
        note: None,
    }
}
