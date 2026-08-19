//! Exif(TIFF)メタデータの抽出。
//!
//! カービングは元のファイル名を取り戻せない(PLAN.md 5.4)。せめて撮影日時と
//! 機種名だけでも拾って連番名に反映するのがこのモジュールの役割で、
//! フェーズ表の Phase 3 後半「Exif メタデータ抽出」がこれにあたる。
//!
//! 対応するのは JPEG の APP1 に入る TIFF 構造(`Exif\0\0` + TIFF ヘッダ)。
//! 読むのは以下のタグだけで、それ以外は読み飛ばす。
//!
//! | タグ | 内容 |
//! |---|---|
//! | 0x010F / 0x0110 | メーカー / 機種 |
//! | 0x0112 | 回転 |
//! | 0x0132, 0x9003, 0x9004 | 日時(撮影日時を優先) |
//! | 0xA002 / 0xA003 | 画素数 |
//!
//! 壊れた Exif で panic しないよう、オフセットは全て範囲確認してから使い、
//! IFD の入れ子は 1 段(ExifIFD)までに限る。

use crate::format::{FileMetadata, Timestamp};
use crate::reader::Reader;

/// 1 つの IFD から読むエントリ数の上限。壊れた count で延々ループしないため。
const MAX_ENTRIES: u16 = 512;
/// 文字列タグの最大長。
const MAX_STRING: usize = 128;

/// JPEG の APP1 セグメント本体を読んで、拾えた項目を `meta` に入れる。
///
/// `seg` はセグメント本体の先頭(長さフィールドの直後)、`len` はその長さ。
/// Exif でなければ何もしない(XMP など他の APP1 もあるため)。
pub(crate) fn read_app1(r: &mut Reader<'_>, seg: u64, len: u64, meta: &mut FileMetadata) {
    if len < 8 || !r.matches(seg, b"Exif\0\0") {
        return;
    }
    read_tiff(r, seg + 6, len - 6, meta);
}

/// TIFF ヘッダから始まる領域を読む。
pub(crate) fn read_tiff(r: &mut Reader<'_>, tiff: u64, len: u64, meta: &mut FileMetadata) {
    let Some(order) = r.array::<2>(tiff) else {
        return;
    };
    let big_endian = match &order {
        b"MM" => true,
        b"II" => false,
        _ => return,
    };
    let t = Tiff {
        base: tiff,
        end: tiff.saturating_add(len),
        big_endian,
    };
    if t.u16(r, tiff + 2) != Some(42) {
        return;
    }
    let Some(ifd0) = t.u32(r, tiff + 4) else {
        return;
    };

    let mut exif_ifd = None;
    let mut datetime = None;
    let mut datetime_original = None;
    let mut datetime_digitized = None;

    t.each_entry(r, u64::from(ifd0), |r, tag, ty, count, value| match tag {
        0x010F => meta.camera_make = t.ascii(r, ty, count, value),
        0x0110 => meta.camera_model = t.ascii(r, ty, count, value),
        0x0112 => meta.orientation = t.short(r, ty, value).filter(|o| (1..=8).contains(o)),
        0x0132 => {
            datetime = t
                .ascii(r, ty, count, value)
                .as_deref()
                .and_then(Timestamp::parse_exif)
        }
        0x8769 => exif_ifd = t.long(r, ty, value),
        _ => {}
    });

    if let Some(off) = exif_ifd {
        t.each_entry(r, u64::from(off), |r, tag, ty, count, value| match tag {
            0x9003 => {
                datetime_original = t
                    .ascii(r, ty, count, value)
                    .as_deref()
                    .and_then(Timestamp::parse_exif)
            }
            0x9004 => {
                datetime_digitized = t
                    .ascii(r, ty, count, value)
                    .as_deref()
                    .and_then(Timestamp::parse_exif)
            }
            0xA002 => meta.width = t.dimension(r, ty, value).or(meta.width),
            0xA003 => meta.height = t.dimension(r, ty, value).or(meta.height),
            _ => {}
        });
    }

    // 撮影日時 > デジタル化日時 > ファイル更新日時 の順で採る。
    meta.timestamp = datetime_original.or(datetime_digitized).or(datetime);
}

/// TIFF の読み出し文脈。IFD のオフセットは全て `base`(TIFF ヘッダ先頭)からの相対。
struct Tiff {
    base: u64,
    end: u64,
    big_endian: bool,
}

impl Tiff {
    fn u16(&self, r: &mut Reader<'_>, at: u64) -> Option<u16> {
        if at.checked_add(2)? > self.end {
            return None;
        }
        if self.big_endian {
            r.u16be(at)
        } else {
            r.u16le(at)
        }
    }

    fn u32(&self, r: &mut Reader<'_>, at: u64) -> Option<u32> {
        if at.checked_add(4)? > self.end {
            return None;
        }
        if self.big_endian {
            r.u32be(at)
        } else {
            r.u32le(at)
        }
    }

    /// IFD のエントリを 1 つずつ渡す。
    ///
    /// `value` はエントリの値フィールド(4 バイト)の位置。値が 4 バイトに
    /// 収まらない場合は、そこに TIFF 先頭からのオフセットが入っている。
    fn each_entry(
        &self,
        r: &mut Reader<'_>,
        ifd_offset: u64,
        mut f: impl FnMut(&mut Reader<'_>, u16, u16, u32, u64),
    ) {
        let ifd = self.base.saturating_add(ifd_offset);
        let Some(count) = self.u16(r, ifd) else {
            return;
        };
        for i in 0..count.min(MAX_ENTRIES) {
            let entry = ifd + 2 + u64::from(i) * 12;
            if entry + 12 > self.end {
                return;
            }
            let (Some(tag), Some(ty), Some(n)) = (
                self.u16(r, entry),
                self.u16(r, entry + 2),
                self.u32(r, entry + 4),
            ) else {
                return;
            };
            f(r, tag, ty, n, entry + 8);
        }
    }

    /// 値フィールドの実体位置。4 バイトを超える値は間接参照になる。
    fn value_at(&self, r: &mut Reader<'_>, size: u64, value: u64) -> Option<u64> {
        if size <= 4 {
            return Some(value);
        }
        let off = self.u32(r, value)?;
        let at = self.base.checked_add(u64::from(off))?;
        (at.checked_add(size)? <= self.end).then_some(at)
    }

    /// ASCII タグ(type 2)を読む。
    fn ascii(&self, r: &mut Reader<'_>, ty: u16, count: u32, value: u64) -> Option<String> {
        if ty != 2 || count == 0 {
            return None;
        }
        let n = (count as usize).min(MAX_STRING);
        let at = self.value_at(r, u64::from(count), value)?;
        let text: String = r
            .view(at, n)
            .iter()
            .copied()
            .take_while(|b| *b != 0)
            .map(|b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    ' '
                }
            })
            .collect();
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    }

    /// SHORT タグ(type 3)を読む。
    fn short(&self, r: &mut Reader<'_>, ty: u16, value: u64) -> Option<u16> {
        (ty == 3).then(|| self.u16(r, value)).flatten()
    }

    /// LONG タグ(type 4)を読む。
    fn long(&self, r: &mut Reader<'_>, ty: u16, value: u64) -> Option<u32> {
        (ty == 4).then(|| self.u32(r, value)).flatten()
    }

    /// 画素数タグ。SHORT でも LONG でも来る。
    fn dimension(&self, r: &mut Reader<'_>, ty: u16, value: u64) -> Option<u32> {
        match ty {
            3 => self.u16(r, value).map(u32::from),
            4 => self.u32(r, value),
            _ => None,
        }
        .filter(|v| *v > 0)
    }
}
