//! 切れた JPEG の続きをグレーで埋める。
//!
//! PLAN.md 5.6 の「途中で切れているケース: デコードできた行までを画像として確定し、
//! 残りはグレーで埋めて保存する。リスタートマーカー(DRI/RSTn)があれば破損箇所
//! 以降のマーカーから再同期して後半も救う」がこれ。
//!
//! やっていることは単純で、**残りの MCU を「係数が全部 0」として符号化し直す**。
//! リスタートマーカーの直後は DC 予測値が 0 に戻るので、DC 差分 0 + EOB を並べれば
//! 係数が全て 0 のブロックになり、逆 DCT とレベルシフトを経て一律 128 の
//! 中間グレーになる。エントロピー符号の途中にビット単位で割り込む必要がないのは
//! リスタートマーカーがバイト境界に揃っているおかげで、DRI の無いファイルでは
//! この手が使えない(切れた所で符号のビット位置が分からなくなるため)。
//!
//! 埋め色は 128 固定になる。[`RepairOptions::fill`](crate::RepairOptions::fill) は
//! PNG 側の設定で、JPEG では原理的に選べない。

use super::scan::{HuffTable, Sof, Sos};

/// 生成するグレー埋めの上限。壊れた寸法で巨大な出力を作らないための歯止め。
const MAX_FILL: usize = 64 * 1024 * 1024;

/// エントロピー符号を書くビット単位のライタ。
///
/// JPEG のエントロピー符号では、生の 0xFF は `FF 00` と書く決まりになっている
/// (マーカーと区別できなくなるため)。
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    bits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            bits: 0,
        }
    }

    /// 上位から `len` ビットを書く。
    fn push(&mut self, code: u16, len: u8) {
        self.acc = (self.acc << u32::from(len)) | u32::from(code);
        self.bits += u32::from(len);
        while self.bits >= 8 {
            self.bits -= 8;
            let byte = ((self.acc >> self.bits) & 0xFF) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0x00);
            }
        }
    }

    /// 端数を 1 で埋めてバイト境界に揃える。
    fn flush(&mut self) {
        if self.bits > 0 {
            let pad = 8 - self.bits;
            self.push((1u16 << pad) - 1, pad as u8);
        }
        self.acc = 0;
        self.bits = 0;
    }

    /// マーカーをそのまま書く(スタッフィングしない)。
    fn marker(&mut self, marker: u8) {
        self.flush();
        self.out.push(0xFF);
        self.out.push(marker);
    }
}

/// 正準ハフマン符号から `symbol` の符号を引く。
///
/// JPEG のハフマン表は「符号長ごとの個数」と「値の並び」しか持たない。
/// 符号そのものは長さ 1 から順に 0 から振っていく決まりで、そこから復元する。
fn code_for(table: &HuffTable, symbol: u8) -> Option<(u16, u8)> {
    let mut code: u32 = 0;
    let mut k = 0usize;
    for len in 1..=16u8 {
        for _ in 0..table.counts[usize::from(len) - 1] {
            let value = *table.symbols.get(k)?;
            if value == symbol {
                return u16::try_from(code).ok().map(|c| (c, len));
            }
            k += 1;
            code += 1;
        }
        code <<= 1;
    }
    None
}

/// 表番号から表を引く。
fn find_table(tables: &[HuffTable], class: u8, id: u8) -> Option<&HuffTable> {
    tables.iter().find(|t| t.class == class && t.id == id)
}

/// 1 ブロック分の「係数が全部 0」を書くための符号。
struct BlockCodes {
    /// DC 差分 0(カテゴリ 0)の符号。
    dc: (u16, u8),
    /// EOB(残りの AC 係数が全部 0)の符号。
    ac: (u16, u8),
    /// この成分が MCU 内に持つブロック数。
    blocks: u32,
}

/// 残りの MCU をグレーとして符号化する。
///
/// `done_intervals` は既に無事に入っているリスタート間隔の数
/// (= 見つかったリスタートマーカーの数)。戻り値はエントロピー符号の続きで、
/// 呼び出し側はこれを繋いだあと EOI を付ける。
///
/// 埋める必要が無い場合と、この手が使えない場合は `None` を返す。
pub(crate) fn gray_tail(
    sof: &Sof,
    sos: &Sos,
    tables: &[HuffTable],
    restart_interval: u16,
    done_intervals: u64,
) -> Option<GrayTail> {
    if restart_interval == 0 || !sof.is_sequential() {
        return None;
    }
    // 成分数が食い違うスキャン(非インターリーブ)は MCU の定義が変わる。
    if sos.components.len() != sof.components.len() {
        return None;
    }

    let interval = u64::from(restart_interval);
    let total_mcus = sof.mcu_count();
    let total_intervals = total_mcus.div_ceil(interval);
    if total_mcus == 0 || done_intervals >= total_intervals {
        return None;
    }

    // 成分ごとに、DC と AC の符号とブロック数を先に引いておく。
    let mut codes = Vec::with_capacity(sos.components.len());
    for sc in &sos.components {
        let comp = sof.components.iter().find(|c| c.id == sc.cs)?;
        codes.push(BlockCodes {
            dc: code_for(find_table(tables, 0, sc.td)?, 0x00)?,
            ac: code_for(find_table(tables, 1, sc.ta)?, 0x00)?,
            blocks: u32::from(comp.h) * u32::from(comp.v),
        });
    }

    let mut w = BitWriter::new();
    let mut filled_mcus = 0u64;
    for i in done_intervals..total_intervals {
        let mcus = if i + 1 == total_intervals {
            total_mcus - i * interval
        } else {
            interval
        };
        for _ in 0..mcus {
            for c in &codes {
                for _ in 0..c.blocks {
                    w.push(c.dc.0, c.dc.1);
                    w.push(c.ac.0, c.ac.1);
                }
            }
        }
        filled_mcus += mcus;
        if i + 1 < total_intervals {
            w.marker(0xD0 + (i % 8) as u8);
        }
        if w.out.len() > MAX_FILL {
            return None;
        }
    }
    w.flush();

    Some(GrayTail {
        data: w.out,
        filled_mcus,
        total_mcus,
    })
}

/// グレー埋めの結果。
pub(crate) struct GrayTail {
    /// エントロピー符号の続き。
    pub data: Vec<u8>,
    /// 埋めた MCU 数。
    pub filled_mcus: u64,
    /// 画像全体の MCU 数。
    pub total_mcus: u64,
}

impl GrayTail {
    /// 埋めた割合(百分率)。
    pub(crate) fn percent(&self) -> u64 {
        if self.total_mcus == 0 {
            return 0;
        }
        self.filled_mcus * 100 / self.total_mcus
    }
}

#[cfg(test)]
mod tests {
    use super::super::tables;
    use super::*;

    fn sof(width: u16, height: u16) -> Sof {
        Sof {
            marker: 0xC0,
            width,
            height,
            components: tables::default_components(),
        }
    }

    fn sos() -> Sos {
        Sos {
            at: 0,
            header_end: 0,
            components: tables::scan_components(&tables::default_components()),
        }
    }

    #[test]
    fn canonical_codes_follow_the_standard_tables() {
        let tables = tables::standard_huffman();
        let dc = find_table(&tables, 0, 0).unwrap();
        // DC 輝度表は長さ 2 の符号が 1 つだけ (値 0) で、符号は 00。
        assert_eq!(code_for(dc, 0x00), Some((0, 2)));
        assert_eq!(code_for(dc, 0x01), Some((2, 3)));
        assert_eq!(code_for(dc, 0xFF), None);

        let ac = find_table(&tables, 1, 0).unwrap();
        // AC 輝度表の EOB は 4 ビット。
        assert_eq!(code_for(ac, 0x00).map(|c| c.1), Some(4));
    }

    #[test]
    fn bit_writer_stuffs_ff_bytes() {
        let mut w = BitWriter::new();
        w.push(0xFF, 8);
        assert_eq!(w.out, vec![0xFF, 0x00]);
        w.marker(0xD0);
        assert_eq!(w.out, vec![0xFF, 0x00, 0xFF, 0xD0]);
    }

    #[test]
    fn fills_the_remaining_intervals() {
        let tables = tables::standard_huffman();
        // 4:2:0 なので MCU は 16x16。32x32 の画像は 4 MCU。
        let sof = sof(32, 32);
        assert_eq!(sof.mcu_count(), 4);

        // リスタート間隔 1 → 4 間隔。2 つ入っているので残り 2 つを埋める。
        let tail = gray_tail(&sof, &sos(), &tables, 1, 2).unwrap();
        assert_eq!(tail.filled_mcus, 2);
        assert_eq!(tail.total_mcus, 4);
        assert_eq!(tail.percent(), 50);
        // 埋めた 2 間隔の間に RST が 1 つだけ入る (最後の間隔の後には入れない)。
        let rst = tail
            .data
            .windows(2)
            .filter(|w| w[0] == 0xFF && (0xD0..=0xD7).contains(&w[1]))
            .count();
        assert_eq!(rst, 1);
    }

    #[test]
    fn refuses_when_restart_markers_are_unavailable() {
        let tables = tables::standard_huffman();
        assert!(gray_tail(&sof(32, 32), &sos(), &tables, 0, 0).is_none());
        // 全部揃っているなら埋めるものがない。
        assert!(gray_tail(&sof(32, 32), &sos(), &tables, 1, 4).is_none());
        // プログレッシブは対象外。
        let mut progressive = sof(32, 32);
        progressive.marker = 0xC2;
        assert!(gray_tail(&progressive, &sos(), &tables, 1, 0).is_none());
    }
}
