//! ヘッダを組み直すための標準テーブルとセグメント生成。
//!
//! 参照ファイルが無い場合、失われた量子化表とハフマン表は ITU-T T.81 Annex K の
//! 標準テーブルで代用する(PLAN.md 5.6「参照がなければ標準量子化テーブル+標準
//! ハフマンテーブルで組み立てて試す」)。
//!
//! 標準表は元のカメラが使った表とは違うので、色や明るさは正確には戻らない。
//! それでも「開けない」から「見られる」には持っていける、という位置づけ。

use super::scan::{Component, HuffTable, ScanComponent};

/// ジグザグ順(DQT はこの順で値を並べる)。
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// 標準の輝度量子化表(Annex K.1、自然順)。
const QUANT_LUMA: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, //
    12, 12, 14, 19, 26, 58, 60, 55, //
    14, 13, 16, 24, 40, 57, 69, 56, //
    14, 17, 22, 29, 51, 87, 80, 62, //
    18, 22, 37, 56, 68, 109, 103, 77, //
    24, 35, 55, 64, 81, 104, 113, 92, //
    49, 64, 78, 87, 103, 121, 120, 101, //
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// 標準の色差量子化表(Annex K.1、自然順)。
const QUANT_CHROMA: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, //
    18, 21, 26, 66, 99, 99, 99, 99, //
    24, 26, 56, 99, 99, 99, 99, 99, //
    47, 66, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// 標準の DC 輝度ハフマン表(Annex K.3)。
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// 標準の DC 色差ハフマン表。
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// 標準の AC 輝度ハフマン表。
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
const AC_LUMA_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// 標準の AC 色差ハフマン表。
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHROMA_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

/// 標準の量子化表 2 つを DQT セグメントとして書き出す。
pub(crate) fn standard_dqt() -> Vec<u8> {
    let mut out = Vec::new();
    for (id, table) in [(0u8, &QUANT_LUMA), (1u8, &QUANT_CHROMA)] {
        out.extend_from_slice(&[0xFF, 0xDB]);
        out.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
        out.push(id); // 8 ビット精度 + 表番号
        for &z in &ZIGZAG {
            out.push(table[z]);
        }
    }
    out
}

/// 標準のハフマン表 4 つ。
pub(crate) fn standard_huffman() -> Vec<HuffTable> {
    vec![
        table(0, 0, DC_LUMA_BITS, &DC_LUMA_VALS),
        table(0, 1, DC_CHROMA_BITS, &DC_CHROMA_VALS),
        table(1, 0, AC_LUMA_BITS, &AC_LUMA_VALS),
        table(1, 1, AC_CHROMA_BITS, &AC_CHROMA_VALS),
    ]
}

fn table(class: u8, id: u8, counts: [u8; 16], symbols: &[u8]) -> HuffTable {
    HuffTable {
        class,
        id,
        counts,
        symbols: symbols.to_vec(),
    }
}

/// ハフマン表を DHT セグメントとして書き出す。
pub(crate) fn dht_segments(tables: &[HuffTable]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in tables {
        let len = 2 + 1 + 16 + t.symbols.len();
        out.extend_from_slice(&[0xFF, 0xC4]);
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.push((t.class << 4) | t.id);
        out.extend_from_slice(&t.counts);
        out.extend_from_slice(&t.symbols);
    }
    out
}

/// 一般的な 3 成分 4:2:0(デジカメ写真の大半がこれ)。
pub(crate) fn default_components() -> Vec<Component> {
    vec![
        Component {
            id: 1,
            h: 2,
            v: 2,
            tq: 0,
        },
        Component {
            id: 2,
            h: 1,
            v: 1,
            tq: 1,
        },
        Component {
            id: 3,
            h: 1,
            v: 1,
            tq: 1,
        },
    ]
}

/// SOF0(ベースライン)セグメントを組み立てる。
pub(crate) fn sof0(width: u16, height: u16, components: &[Component]) -> Vec<u8> {
    let len = 8 + components.len() * 3;
    let mut out = vec![0xFF, 0xC0];
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.push(8); // 標本精度
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.push(components.len() as u8);
    for c in components {
        out.push(c.id);
        out.push((c.h << 4) | c.v);
        out.push(c.tq);
    }
    out
}

/// SOS セグメントを組み立てる。
pub(crate) fn sos(components: &[ScanComponent]) -> Vec<u8> {
    let len = 6 + components.len() * 2;
    let mut out = vec![0xFF, 0xDA];
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.push(components.len() as u8);
    for c in components {
        out.push(c.cs);
        out.push((c.td << 4) | c.ta);
    }
    // ベースラインの固定値(Ss=0, Se=63, Ah=0, Al=0)。
    out.extend_from_slice(&[0x00, 0x3F, 0x00]);
    out
}

/// SOF の成分並びに対応する既定のスキャン成分(輝度は表 0、色差は表 1)。
pub(crate) fn scan_components(components: &[Component]) -> Vec<ScanComponent> {
    components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let t = u8::from(i > 0);
            ScanComponent {
                cs: c.id,
                td: t,
                ta: t,
            }
        })
        .collect()
}

/// DRI セグメント。
pub(crate) fn dri(interval: u16) -> Vec<u8> {
    let mut out = vec![0xFF, 0xDD, 0x00, 0x04];
    out.extend_from_slice(&interval.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_counts_match_the_value_lists() {
        for t in standard_huffman() {
            let total: usize = t.counts.iter().map(|c| usize::from(*c)).sum();
            assert_eq!(total, t.symbols.len(), "class {} id {}", t.class, t.id);
        }
    }

    #[test]
    fn zigzag_is_a_permutation() {
        let mut seen = [false; 64];
        for &z in &ZIGZAG {
            assert!(!seen[z], "{z} が 2 回出ている");
            seen[z] = true;
        }
        assert!(seen.iter().all(|s| *s));
    }

    #[test]
    fn dqt_segments_have_the_declared_length() {
        let dqt = standard_dqt();
        assert_eq!(dqt.len(), 2 * (2 + 2 + 1 + 64));
        assert_eq!(&dqt[..2], &[0xFF, 0xDB]);
        assert_eq!(u16::from_be_bytes([dqt[2], dqt[3]]), 67);
        // ジグザグ順の先頭は自然順の (0,0)。
        assert_eq!(dqt[5], QUANT_LUMA[0]);
    }

    #[test]
    fn sof_and_sos_lengths_agree_with_the_component_count() {
        let comps = default_components();
        let sof = sof0(640, 480, &comps);
        assert_eq!(u16::from_be_bytes([sof[2], sof[3]]) as usize, sof.len() - 2);
        let sos = sos(&scan_components(&comps));
        assert_eq!(u16::from_be_bytes([sos[2], sos[3]]) as usize, sos.len() - 2);
    }
}
