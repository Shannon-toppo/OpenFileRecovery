//! JPEG のマーカー構造を読む。
//!
//! 相手は壊れたファイルなので、途中で辻褄が合わなくなったら**そこまでで打ち切って
//! 分かったことだけ返す**。エラーにはしない。何が残っていて何が失われたかを
//! 呼び出し側が判断するための材料を集めるのがここの仕事。

/// SOI を探す範囲。これより後ろに SOI があっても、それは別のファイルの先頭とみなす。
const SOI_SEARCH: usize = 64 * 1024;

/// SOF に入っている 1 成分の情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Component {
    /// 成分 ID。
    pub id: u8,
    /// 水平サンプリング係数。
    pub h: u8,
    /// 垂直サンプリング係数。
    pub v: u8,
    /// 使う量子化表の番号。
    pub tq: u8,
}

/// フレームヘッダ(SOF)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sof {
    /// SOF マーカーの種類(0xC0 なら baseline、0xC2 なら progressive)。
    pub marker: u8,
    /// 画像の幅。
    pub width: u16,
    /// 画像の高さ。
    pub height: u16,
    /// 成分。
    pub components: Vec<Component>,
}

impl Sof {
    /// ベースライン / 拡張シーケンシャルか。プログレッシブはグレー埋めの対象外。
    pub(crate) fn is_sequential(&self) -> bool {
        matches!(self.marker, 0xC0 | 0xC1)
    }

    /// 最大サンプリング係数(MCU の大きさを決める)。
    pub(crate) fn max_sampling(&self) -> (u32, u32) {
        let h = self
            .components
            .iter()
            .map(|c| u32::from(c.h))
            .max()
            .unwrap_or(1);
        let v = self
            .components
            .iter()
            .map(|c| u32::from(c.v))
            .max()
            .unwrap_or(1);
        (h.max(1), v.max(1))
    }

    /// 画像全体の MCU 数。
    pub(crate) fn mcu_count(&self) -> u64 {
        let (hmax, vmax) = self.max_sampling();
        let across = u64::from(self.width).div_ceil(u64::from(8 * hmax));
        let down = u64::from(self.height).div_ceil(u64::from(8 * vmax));
        across * down
    }
}

/// スキャンヘッダ(SOS)に入っている 1 成分の情報。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScanComponent {
    /// 成分 ID(SOF の `id` に対応する)。
    pub cs: u8,
    /// 使う DC ハフマン表の番号。
    pub td: u8,
    /// 使う AC ハフマン表の番号。
    pub ta: u8,
}

/// スキャンヘッダ(SOS)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sos {
    /// マーカー(0xFF)の位置。
    pub at: usize,
    /// ヘッダの終わり = エントロピー符号の始まり。
    pub header_end: usize,
    /// 成分。
    pub components: Vec<ScanComponent>,
}

/// ハフマン表(DHT の中身 1 つ)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HuffTable {
    /// 0 なら DC、1 なら AC。
    pub class: u8,
    /// 表番号。
    pub id: u8,
    /// 符号長ごとの個数。
    pub counts: [u8; 16],
    /// 値の並び。
    pub symbols: Vec<u8>,
}

/// ファイル中のバイト範囲。
pub(crate) type Range = (usize, usize);

/// 読み取れた JPEG の構造。
#[derive(Debug, Default, Clone)]
pub(crate) struct Jpeg {
    /// SOI の位置。前にごみが付いていることがあるので位置で持つ。
    pub soi: Option<usize>,
    /// フレームヘッダ。
    pub sof: Option<Sof>,
    /// 最初のスキャンヘッダ。
    pub sos: Option<Sos>,
    /// DQT セグメントの範囲。
    pub dqt: Vec<Range>,
    /// DHT セグメントの範囲。
    pub dht: Vec<Range>,
    /// DHT の中身。
    pub huffman: Vec<HuffTable>,
    /// DRI セグメントの範囲。
    pub dri_range: Option<Range>,
    /// リスタート間隔(MCU 数)。0 なら無効。
    pub dri: Option<u16>,
    /// APPn / COM セグメントの範囲。ヘッダを組み直すときに引き継ぐ。
    pub app: Vec<Range>,
    /// EOI(0xFF)の位置。
    pub eoi: Option<usize>,
    /// エントロピー符号の終わり(EOI の位置、または読めた末尾)。
    pub entropy_end: usize,
    /// 見つかったリスタートマーカーの数。
    pub rst_count: u64,
    /// 最後のリスタートマーカー(0xFF)の位置。
    pub last_rst: Option<usize>,
    /// スキャン(SOS)の数。2 以上ならプログレッシブなど多重スキャン。
    pub scans: usize,
    /// Exif から拾えた寸法。
    pub exif_size: Option<(u32, u32)>,
    /// 構造を最後まで辻褄が合う形で辿れたか。
    pub walked_to_end: bool,
}

impl Jpeg {
    /// ヘッダ(SOI + 量子化表 + フレームヘッダ + ハフマン表 + スキャンヘッダ)が
    /// 揃っていて、そのまま使えるか。
    pub(crate) fn header_is_usable(&self) -> bool {
        self.soi.is_some()
            && self.sof.is_some()
            && self.sos.is_some()
            && !self.dqt.is_empty()
            && !self.dht.is_empty()
    }

    /// エントロピー符号の始まり。
    pub(crate) fn entropy_start(&self) -> Option<usize> {
        self.sos.as_ref().map(|s| s.header_end)
    }

    /// 見つからなかったものを、別の走査結果から埋める。
    ///
    /// 通常の走査は SOI から順に辿るので、途中が潰れているとその先を諦める。
    /// 残骸拾い([`scan_orphans`])の結果をここで合流させると、
    /// 「ヘッダの真ん中だけが壊れている」ファイルからも本体を拾える。
    pub(crate) fn fill_gaps_from(&mut self, other: Jpeg) {
        if self.dqt.is_empty() {
            self.dqt = other.dqt;
        }
        if self.dht.is_empty() {
            self.dht = other.dht;
            self.huffman = other.huffman;
        }
        if self.sof.is_none() {
            self.sof = other.sof;
        }
        if self.dri.is_none() {
            self.dri = other.dri;
            self.dri_range = other.dri_range;
        }
        if self.exif_size.is_none() {
            self.exif_size = other.exif_size;
        }
        if self.sos.is_none() && other.sos.is_some() {
            // スキャンヘッダを借りたら、その先のエントロピー符号の情報も一式借りる。
            self.sos = other.sos;
            self.scans = other.scans;
            self.rst_count = other.rst_count;
            self.last_rst = other.last_rst;
            self.eoi = other.eoi;
            self.entropy_end = other.entropy_end;
        }
    }
}

/// JPEG の構造を読む。
pub(crate) fn scan(data: &[u8]) -> Jpeg {
    let mut out = Jpeg {
        entropy_end: data.len(),
        ..Jpeg::default()
    };

    let Some(soi) = find_soi(data) else {
        // SOI が無い = 先頭が失われている。エントロピー符号だけが残っている形。
        return out;
    };
    out.soi = Some(soi);

    let mut pos = soi + 2;
    while pos + 2 <= data.len() {
        if data[pos] != 0xFF {
            break;
        }
        let marker = data[pos + 1];
        match marker {
            // フィルバイト。
            0xFF => {
                pos += 1;
                continue;
            }
            // 長さフィールドを持たないマーカー。
            0x01 | 0xD0..=0xD7 => {
                pos += 2;
                continue;
            }
            0xD8 => {
                pos += 2;
                continue;
            }
            0xD9 => {
                out.eoi = Some(pos);
                out.entropy_end = pos;
                out.walked_to_end = true;
                break;
            }
            _ => {}
        }

        let Some(len) = be16(data, pos + 2) else {
            break;
        };
        let len = usize::from(len);
        if len < 2 || pos + 2 + len > data.len() {
            break;
        }
        let body = pos + 4;
        let body_len = len - 2;
        let range = (pos, pos + 2 + len);

        match marker {
            0xDB => out.dqt.push(range),
            0xC4 => {
                out.dht.push(range);
                read_dht(&data[body..body + body_len], &mut out.huffman);
            }
            0xDD => {
                out.dri = be16(data, body);
                out.dri_range = Some(range);
            }
            0xE0..=0xEF | 0xFE => {
                out.app.push(range);
                if marker == 0xE1 && out.exif_size.is_none() {
                    out.exif_size = exif_size(&data[body..body + body_len]);
                }
            }
            0xDA => {
                out.scans += 1;
                if out.sos.is_none()
                    && let Some(sos) = read_sos(data, pos, body, body_len)
                {
                    out.sos = Some(sos);
                }
                // エントロピー符号を読み飛ばして次のマーカーへ。
                pos = pos + 2 + len;
                match skip_entropy(data, pos, &mut out) {
                    Some(next) => {
                        pos = next;
                        continue;
                    }
                    None => break,
                }
            }
            m if is_sof(m) => {
                if out.sof.is_none()
                    && let Some(sof) = read_sof(m, &data[body..body + body_len])
                {
                    out.sof = Some(sof);
                }
            }
            _ => {}
        }
        pos += 2 + len;
    }

    out
}

/// SOI を探す。先頭にごみが付いたファイルのために少しだけ後ろも見る。
fn find_soi(data: &[u8]) -> Option<usize> {
    let limit = data.len().min(SOI_SEARCH);
    (0..limit.saturating_sub(2))
        .find(|&i| data[i] == 0xFF && data[i + 1] == 0xD8 && data.get(i + 2) == Some(&0xFF))
}

/// SOF マーカーか。DHT(C4)/ JPG(C8)/ DAC(CC)は除く。
fn is_sof(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]))
}

/// エントロピー符号を読み飛ばし、次の本物のマーカー位置を返す。
///
/// ついでにリスタートマーカーを数える。切り詰められたファイルの
/// 「どこまでが無事か」はこの数から決まる。
fn skip_entropy(data: &[u8], from: usize, out: &mut Jpeg) -> Option<usize> {
    let mut pos = from;
    while pos + 1 < data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        match data[pos + 1] {
            // バイトスタッフィング。データ中の 0xFF は FF 00 と書かれる。
            0x00 => pos += 2,
            // リスタートマーカー。
            0xD0..=0xD7 => {
                out.rst_count += 1;
                out.last_rst = Some(pos);
                pos += 2;
            }
            // フィルバイト。
            0xFF => pos += 1,
            // 本物のマーカー。
            _ => return Some(pos),
        }
    }
    None
}

/// SOF の中身を読む。
fn read_sof(marker: u8, body: &[u8]) -> Option<Sof> {
    if body.len() < 6 {
        return None;
    }
    let height = u16::from_be_bytes([body[1], body[2]]);
    let width = u16::from_be_bytes([body[3], body[4]]);
    let count = usize::from(body[5]);
    if count == 0 || body.len() < 6 + count * 3 {
        return None;
    }
    let components = (0..count)
        .map(|i| {
            let at = 6 + i * 3;
            Component {
                id: body[at],
                h: (body[at + 1] >> 4).max(1),
                v: (body[at + 1] & 0x0F).max(1),
                tq: body[at + 2],
            }
        })
        .collect();
    Some(Sof {
        marker,
        width,
        height,
        components,
    })
}

/// SOS の中身を読む。
fn read_sos(data: &[u8], at: usize, body: usize, body_len: usize) -> Option<Sos> {
    let body = data.get(body..body + body_len)?;
    let count = usize::from(*body.first()?);
    if count == 0 || body.len() < 1 + count * 2 {
        return None;
    }
    let components = (0..count)
        .map(|i| {
            let p = 1 + i * 2;
            ScanComponent {
                cs: body[p],
                td: body[p + 1] >> 4,
                ta: body[p + 1] & 0x0F,
            }
        })
        .collect();
    Some(Sos {
        at,
        header_end: at + 2 + 2 + body_len,
        components,
    })
}

/// DHT の中身(1 セグメントに複数の表が入ることがある)を読む。
fn read_dht(mut body: &[u8], out: &mut Vec<HuffTable>) {
    while body.len() >= 17 {
        let class = body[0] >> 4;
        let id = body[0] & 0x0F;
        let mut counts = [0u8; 16];
        counts.copy_from_slice(&body[1..17]);
        let total: usize = counts.iter().map(|c| usize::from(*c)).sum();
        if body.len() < 17 + total {
            return;
        }
        out.push(HuffTable {
            class,
            id,
            counts,
            symbols: body[17..17 + total].to_vec(),
        });
        body = &body[17 + total..];
    }
}

/// APP1 の Exif から画素数(0xA002 / 0xA003)を拾う。
///
/// ヘッダを組み直すとき、SOF が失われていても寸法だけはここから分かることがある。
/// ofr-carve の Exif 抽出とは読む相手(スライス vs デバイス)が違うので別実装だが、
/// 見るタグは 2 つだけなのでこれで足りる。
fn exif_size(body: &[u8]) -> Option<(u32, u32)> {
    let tiff = body.strip_prefix(b"Exif\0\0")?;
    let big_endian = match tiff.get(..2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    if u16at(2)? != 42 {
        return None;
    }

    // IFD0 を見て、ExifIFD (0x8769) があればそこも見る。入れ子は 1 段まで。
    let mut width = None;
    let mut height = None;
    let mut ifds = [Some(u32at(4)? as usize), None];
    let mut index = 0;
    while index < ifds.len() {
        let Some(ifd) = ifds[index] else { break };
        index += 1;
        let Some(entries) = u16at(ifd) else { continue };
        for i in 0..usize::from(entries.min(512)) {
            let at = ifd + 2 + i * 12;
            let (Some(tag), Some(ty), Some(value)) = (u16at(at), u16at(at + 2), u32at(at + 8))
            else {
                break;
            };
            // SHORT (3) は 4 バイト欄の頭 2 バイトに入る。
            let short = u16at(at + 8).map(u32::from);
            let scalar = match ty {
                3 => short,
                4 => Some(value),
                _ => None,
            };
            match tag {
                0x8769 => ifds[1] = Some(value as usize),
                // PixelXDimension / PixelYDimension、無ければ ImageWidth / ImageLength。
                0xA002 => width = scalar.or(width),
                0xA003 => height = scalar.or(height),
                0x0100 if width.is_none() => width = scalar,
                0x0101 if height.is_none() => height = scalar,
                _ => {}
            }
        }
    }
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// SOI が失われたファイルから、生き残っているセグメントを拾い集める。
///
/// ヘッダが壊れているのに本体が残っているケース(PLAN.md 5.6)で使う。
/// エントロピー符号の中では生の 0xFF が必ず `FF 00` に化けているので、
/// 素のマーカー並びを探すやり方は見た目より安全に効く。
pub(crate) fn scan_orphans(data: &[u8]) -> Jpeg {
    let mut out = Jpeg {
        entropy_end: data.len(),
        ..Jpeg::default()
    };

    let limit = data.len().min(SOI_SEARCH);
    let mut pos = 0usize;
    while pos + 4 <= limit {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        let Some(len) = be16(data, pos + 2).map(usize::from) else {
            break;
        };
        let end = pos + 2 + len;
        if len < 2 || end > data.len() {
            pos += 1;
            continue;
        }
        let body = pos + 4;
        let body_len = len - 2;
        let range = (pos, end);

        match marker {
            0xDB if out.dqt.is_empty() && body_len % 65 == 0 => out.dqt.push(range),
            0xC4 if out.dht.is_empty() && body_len >= 17 => {
                let before = out.huffman.len();
                read_dht(&data[body..end], &mut out.huffman);
                if out.huffman.len() > before {
                    out.dht.push(range);
                }
            }
            0xDD if out.dri.is_none() && body_len == 2 => {
                out.dri = be16(data, body);
                out.dri_range = Some(range);
            }
            0xE1 if out.exif_size.is_none() => {
                if let Some(size) = exif_size(&data[body..end]) {
                    out.exif_size = Some(size);
                    out.app.push(range);
                }
            }
            m if is_sof(m) && out.sof.is_none() => {
                // 精度 8 ビット・成分 1〜4 のものだけを本物とみなす。
                if body_len >= 6
                    && data[body] == 8
                    && (1..=4).contains(&data[body + 5])
                    && let Some(sof) = read_sof(m, &data[body..end])
                {
                    out.sof = Some(sof);
                }
            }
            0xDA if out.sos.is_none() => {
                // 末尾の Ss / Se / Ah,Al が仕様どおりかで偶然の一致を落とす。
                let ok = body_len >= 6
                    && (1..=4).contains(&data[body])
                    && usize::from(data[body]) * 2 + 4 == body_len
                    && data[end - 3] <= 63
                    && data[end - 2] <= 63;
                if ok && let Some(sos) = read_sos(data, pos, body, body_len) {
                    out.scans += 1;
                    // ここから先はエントロピー符号。リスタートマーカーを数える。
                    let entropy = sos.header_end;
                    out.sos = Some(sos);
                    if let Some(next) = skip_entropy(data, entropy, &mut out)
                        && data.get(next + 1) == Some(&0xD9)
                    {
                        out.eoi = Some(next);
                        out.entropy_end = next;
                    }
                    return out;
                }
            }
            _ => {}
        }
        // 長さフィールドを持つ既知のマーカーだった場合だけ、セグメントごと飛ばす。
        // それ以外は 1 バイトずらして探し直す(偶然 0xFF が並んだだけのことがある)。
        pos = if matches!(marker, 0xC0..=0xCF | 0xDA | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE) {
            end.max(pos + 1)
        } else {
            pos + 1
        };
    }
    out
}
