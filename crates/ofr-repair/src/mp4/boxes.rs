//! ISO-BMFF のボックスを読み書きする小道具。
//!
//! ボックスは「長さ(4) + 種別(4) + 中身」の入れ子で、長さが自己記述なので
//! 構造だけなら簡単に辿れる。壊れた長さで暴走しないよう、境界は必ず確かめる。

/// メモリ上のボックス列から、`tag` の中身(ヘッダを除く)を探す。
pub(crate) fn find<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    walk(data).find(|b| &b.tag == tag).map(|b| b.body)
}

/// メモリ上のボックス列から、`tag` のボックス全体(ヘッダを含む)を探す。
pub(crate) fn find_whole<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    walk(data).find(|b| &b.tag == tag).map(|b| b.whole)
}

/// 走査中のボックス 1 つ。
pub(crate) struct Found<'a> {
    /// 種別。
    pub tag: [u8; 4],
    /// 中身(ヘッダを除く)。
    pub body: &'a [u8],
    /// ヘッダを含むボックス全体。
    pub whole: &'a [u8],
}

/// ボックス列を順に返す。辻褄が合わなくなった所で止まる。
pub(crate) fn walk(data: &[u8]) -> impl Iterator<Item = Found<'_>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos + 8 > data.len() {
            return None;
        }
        let size32 = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        let tag: [u8; 4] = data[pos + 4..pos + 8].try_into().ok()?;
        if !tag.iter().all(|b| (0x20..=0x7E).contains(b)) {
            return None;
        }
        let (header, size) = match size32 {
            // 0 は「以降すべて」。
            0 => (8usize, data.len() - pos),
            1 => {
                let big = u64::from_be_bytes(data.get(pos + 8..pos + 16)?.try_into().ok()?);
                (16usize, usize::try_from(big).ok()?)
            }
            n => (8usize, n as usize),
        };
        if size < header || pos + size > data.len() {
            return None;
        }
        let found = Found {
            tag,
            body: &data[pos + header..pos + size],
            whole: &data[pos..pos + size],
        };
        pos += size;
        Some(found)
    })
}

/// ボックスを 1 つ組み立てる。
pub(crate) fn make(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    out
}

/// 複数の子ボックスを連結して 1 つの入れ物にする。
pub(crate) fn container(tag: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for c in children {
        body.extend_from_slice(c);
    }
    make(tag, &body)
}

pub(crate) fn be32(data: &[u8], at: usize) -> u32 {
    match data.get(at..at + 4) {
        Some(b) => u32::from_be_bytes(b.try_into().unwrap()),
        None => 0,
    }
}

pub(crate) fn be64(data: &[u8], at: usize) -> u64 {
    match data.get(at..at + 8) {
        Some(b) => u64::from_be_bytes(b.try_into().unwrap()),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_nested_boxes() {
        let inner = make(b"mvhd", &[1, 2, 3]);
        let moov = container(b"moov", std::slice::from_ref(&inner));
        let file = [make(b"ftyp", b"isom"), moov].concat();

        let tags: Vec<[u8; 4]> = walk(&file).map(|b| b.tag).collect();
        assert_eq!(tags, vec![*b"ftyp", *b"moov"]);
        let moov_body = find(&file, b"moov").unwrap();
        assert_eq!(find(moov_body, b"mvhd"), Some(&[1u8, 2, 3][..]));
        assert_eq!(find_whole(moov_body, b"mvhd"), Some(&inner[..]));
    }

    #[test]
    fn stops_on_broken_sizes() {
        // 長さがファイルを越えている。
        let mut bad = make(b"mdat", &[0u8; 4]);
        bad[3] = 0xFF;
        assert_eq!(walk(&bad).count(), 0);

        // 種別が ASCII でない。
        let junk = [
            0u8, 0, 0, 16, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(walk(&junk).count(), 0);
    }
}
