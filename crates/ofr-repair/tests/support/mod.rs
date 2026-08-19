//! 破損サンプル集の生成(PLAN.md 9章)。
//!
//! 正常な JPEG / PNG / AVI / MP4 を機械生成し、そこから「ヘッダ破壊」「途中切断」
//! 「moov 削除」「idx1 削除」などを作る。実機で壊れたファイルは CI に置けないので、
//! 壊し方をコードで定義しておくのが回帰テストの土台になる。
//!
//! 静止画は `image` クレートで実際にエンコードした本物を使う。修復結果を
//! デコードして元の絵と見比べられるので、「開けるようになった」だけでなく
//! 「中身が保たれている」ところまで確かめられる。
//!
//! 動画は手で組み立てる。中身の画素は詰め物だが、構造(ボックス、チャンク、
//! 長さ接頭辞付き NAL ユニット)は本物と同じ形にしてある。修復が見るのは
//! 構造だけなのでこれで足りる。

#![allow(dead_code)] // テストごとに使う組み合わせが違う。

use std::io::Cursor;

// ---------------------------------------------------------------- 静止画

/// 見て分かる絵の付いた RGB 画像。修復前後の比較に使う。
pub fn photo(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        // 斜めの縞と隅のマーカー。全域が一色にならないようにしてある。
        let stripe = if (x / 8 + y / 8) % 2 == 0 { 40 } else { 200 };
        let r = (x * 255 / width.max(1)) as u8;
        let g = (y * 255 / height.max(1)) as u8;
        image::Rgb([r, g, stripe])
    })
}

/// 正常な JPEG。
pub fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let img = photo(width, height);
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
        .expect("JPEG のエンコード");
    out
}

/// 正常な PNG。
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let img = photo(width, height);
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("PNG のエンコード");
    out
}

/// デコードして画素を取り出す。開けなければ `None`。
pub fn decode(data: &[u8]) -> Option<image::RgbImage> {
    image::ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
        .map(|i| i.to_rgb8())
}

/// 2 枚の画像がどれくらい違うか(画素あたりの平均差)。
///
/// JPEG は非可逆なので完全一致はしない。「同じ絵かどうか」はこの値で見る。
pub fn mean_difference(a: &image::RgbImage, b: &image::RgbImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions(), "寸法が違う");
    let total: u64 = a
        .pixels()
        .zip(b.pixels())
        .map(|(p, q)| {
            p.0.iter()
                .zip(q.0.iter())
                .map(|(x, y)| u64::from(x.abs_diff(*y)))
                .sum::<u64>()
        })
        .sum();
    total as f64 / (a.width() as f64 * a.height() as f64 * 3.0)
}

// ---------------------------------------------------------------- 壊し方

/// 先頭 `bytes` バイトを 0 で潰す(ヘッダ破壊)。
pub fn destroy_head(data: &[u8], bytes: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    for b in out.iter_mut().take(bytes.min(data.len())) {
        *b = 0;
    }
    out
}

/// 末尾を切り落とす(途中切断)。`keep` は残す割合。
pub fn truncate(data: &[u8], keep: f64) -> Vec<u8> {
    let n = ((data.len() as f64) * keep) as usize;
    data[..n.min(data.len())].to_vec()
}

/// 末尾にごみを足す。
pub fn append_junk(data: &[u8], bytes: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    out.extend((0..bytes).map(|i| (i * 37 % 251) as u8));
    out
}

/// `tag` から始まる 4 バイトを別の名前に書き換えて、そのチャンク / ボックスを消す。
///
/// 長さはそのままなので「読み飛ばされる」形になる。実際の破損でも、
/// 名前だけが化けて中身が読めなくなることはよくある。
pub fn rename_tag(data: &[u8], tag: &[u8; 4], to: &[u8; 4]) -> Vec<u8> {
    let mut out = data.to_vec();
    if let Some(at) = find(&out, tag) {
        out[at..at + 4].copy_from_slice(to);
    }
    out
}

/// ISO-BMFF の最上位ボックスを 1 つ丸ごと取り除く。
pub fn remove_box(data: &[u8], tag: &[u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let name: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let size = if size == 0 { data.len() - pos } else { size };
        if size < 8 || pos + size > data.len() {
            out.extend_from_slice(&data[pos..]);
            return out;
        }
        if &name != tag {
            out.extend_from_slice(&data[pos..pos + size]);
        }
        pos += size;
    }
    out.extend_from_slice(&data[pos..]);
    out
}

/// RIFF の最上位チャンクを 1 つ丸ごと取り除き、RIFF のサイズも詰める。
pub fn remove_riff_chunk(data: &[u8], tag: &[u8; 4]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"AVI ");
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let name: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        // LIST の場合は中の種別 (hdrl / movi) で判定する。
        let inner: Option<[u8; 4]> = data.get(pos + 8..pos + 12).map(|s| s.try_into().unwrap());
        let matched = &name == tag || (&name == b"LIST" && inner.as_ref() == Some(tag));
        let total = 8 + size + (size & 1);
        if pos + total > data.len() {
            break;
        }
        if !matched {
            body.extend_from_slice(&data[pos..pos + total]);
        }
        pos += total;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// PNG チャンクの CRC を 1 件壊す。
pub fn break_png_crc(data: &[u8], tag: &[u8; 4]) -> Vec<u8> {
    let mut out = data.to_vec();
    if let Some(at) = find(&out, tag) {
        let len = u32::from_be_bytes(out[at - 4..at].try_into().unwrap()) as usize;
        let crc = at + 4 + len;
        if crc + 4 <= out.len() {
            out[crc] ^= 0xFF;
        }
    }
    out
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------- AVI

/// 正常な AVI(MJPG、`frames` フレーム)。
///
/// hdrl(avih + strl)、movi、idx1 を揃えた最小構成。
pub fn avi(frames: u32) -> Vec<u8> {
    let (width, height) = (320u32, 240u32);

    let mut avih = Vec::new();
    push32(&mut avih, 33_333); // マイクロ秒/フレーム (30fps)
    push32(&mut avih, 1_000_000); // 最大転送量
    push32(&mut avih, 0); // パディング
    push32(&mut avih, 0x10); // AVIF_HASINDEX
    push32(&mut avih, frames);
    push32(&mut avih, 0); // 先頭フレーム
    push32(&mut avih, 1); // ストリーム数
    push32(&mut avih, 4096); // 推奨バッファ
    push32(&mut avih, width);
    push32(&mut avih, height);
    avih.extend_from_slice(&[0u8; 16]);

    let mut strh = Vec::new();
    strh.extend_from_slice(b"vids");
    strh.extend_from_slice(b"MJPG");
    push32(&mut strh, 0); // フラグ
    push32(&mut strh, 0); // 優先度 + 言語
    push32(&mut strh, 0); // 先頭フレーム
    push32(&mut strh, 1); // スケール
    push32(&mut strh, 30); // レート → 30fps
    push32(&mut strh, 0); // 開始
    push32(&mut strh, frames); // 長さ
    push32(&mut strh, 4096); // 推奨バッファ
    push32(&mut strh, 0xFFFF_FFFF); // 品質
    push32(&mut strh, 0); // 標本サイズ (0 = 1 チャンク 1 標本)
    strh.extend_from_slice(&[0u8; 8]); // rcFrame

    let mut strf = Vec::new();
    push32(&mut strf, 40); // biSize
    push32(&mut strf, width);
    push32(&mut strf, height);
    strf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    strf.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    strf.extend_from_slice(b"MJPG"); // biCompression
    push32(&mut strf, width * height * 3);
    strf.extend_from_slice(&[0u8; 16]);

    let strl = list(
        b"strl",
        &[chunk(b"strh", &strh), chunk(b"strf", &strf)].concat(),
    );
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), strl].concat());

    // movi の中身と、そのままインデックスにできる位置の一覧。
    let mut movi_body = Vec::new();
    movi_body.extend_from_slice(b"movi");
    let mut index = Vec::new();
    for i in 0..frames {
        let payload = filler(200 + (i as usize % 7) * 16, i as u8);
        // idx1 の位置は "movi" の 4 文字を 0 とした値。
        index.push((movi_body.len() as u32, payload.len() as u32));
        movi_body.extend_from_slice(&chunk(b"00dc", &payload));
    }
    let movi = list_raw(&movi_body);

    let mut idx1 = Vec::new();
    for (offset, size) in &index {
        idx1.extend_from_slice(b"00dc");
        push32(&mut idx1, 0x10); // AVIIF_KEYFRAME
        push32(&mut idx1, *offset);
        push32(&mut idx1, *size);
    }

    let body = [b"AVI ".to_vec(), hdrl, movi, chunk(b"idx1", &idx1)].concat();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    push32(&mut out, body.len() as u32);
    out.extend_from_slice(&body);
    out
}

fn chunk(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 9);
    out.extend_from_slice(tag);
    push32(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn list(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut inner = tag.to_vec();
    inner.extend_from_slice(body);
    list_raw(&inner)
}

fn list_raw(inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"LIST");
    push32(&mut out, inner.len() as u32);
    out.extend_from_slice(inner);
    out
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn filler(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (seed.wrapping_add(i as u8) & 0x5F) | 0x20)
        .collect()
}

// ---------------------------------------------------------------- MP4

/// 正常な MP4(H.264、`frames` フレーム)。
///
/// `mdat` の中は長さ接頭辞付きの NAL ユニットで、10 フレームごとに
/// SPS + PPS + IDR が入る。修復側はこの並びを辿って索引を作り直す。
pub fn mp4(frames: u32) -> Vec<u8> {
    let timescale = 30_000u32;
    let frame_duration = 1001u32; // 29.97fps

    // ---- mdat の中身 ----
    let mut mdat_body = Vec::new();
    let mut samples: Vec<(u32, u32, bool)> = Vec::new(); // (位置, 大きさ, キーフレーム)
    for i in 0..frames {
        let start = mdat_body.len() as u32;
        let key = i % 10 == 0;
        if key {
            mdat_body.extend(nal(7, false, 20)); // SPS
            mdat_body.extend(nal(8, false, 8)); // PPS
            mdat_body.extend(nal(5, true, 400)); // IDR スライス
        } else {
            mdat_body.extend(nal(1, true, 120 + (i as usize % 5) * 16));
        }
        samples.push((start, mdat_body.len() as u32 - start, key));
    }

    // ---- moov ----
    // 位置は「ftyp + moov + mdat ヘッダ」の後ろから始まる。moov の大きさは
    // 位置の値によらないので、一度組んで測ってからずらす。
    let ftyp = mp4_ftyp();
    let probe = mp4_moov(&samples, 0, timescale, frame_duration);
    let mdat_start = (ftyp.len() + probe.len() + 8) as u32;
    let moov = mp4_moov(&samples, mdat_start, timescale, frame_duration);
    assert_eq!(moov.len(), probe.len());

    let mut out = Vec::new();
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&((mdat_body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&mdat_body);
    out
}

/// 長さ接頭辞付きの NAL ユニット 1 つ。
fn nal(kind: u8, first_mb_zero: bool, payload: usize) -> Vec<u8> {
    let mut body = vec![kind & 0x1F];
    // スライスヘッダの先頭ビット。1 なら「画面の先頭から始まる」= フレームの頭。
    body.push(if first_mb_zero { 0x88 } else { 0x08 });
    body.extend(filler(payload, kind));
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend(body);
    out
}

fn mp4_ftyp() -> Vec<u8> {
    bx(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41")
}

fn mp4_moov(samples: &[(u32, u32, bool)], base: u32, timescale: u32, delta: u32) -> Vec<u8> {
    let n = samples.len() as u32;
    let media_duration = u64::from(n) * u64::from(delta);
    let movie_duration = media_duration * 1000 / u64::from(timescale);

    let mut mvhd = vec![0, 0, 0, 0];
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&(movie_duration as u32).to_be_bytes());
    mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 10]);
    mvhd.extend_from_slice(&IDENTITY);
    mvhd.extend_from_slice(&[0u8; 24]);
    mvhd.extend_from_slice(&2u32.to_be_bytes());

    let mut tkhd = vec![0, 0, 0, 7];
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&0u32.to_be_bytes());
    tkhd.extend_from_slice(&1u32.to_be_bytes()); // トラック ID
    tkhd.extend_from_slice(&[0u8; 4]);
    tkhd.extend_from_slice(&(movie_duration as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]);
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // レイヤー
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // 代替グループ
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // 音量
    tkhd.extend_from_slice(&[0u8; 2]);
    tkhd.extend_from_slice(&IDENTITY);
    tkhd.extend_from_slice(&(1280u32 << 16).to_be_bytes());
    tkhd.extend_from_slice(&(720u32 << 16).to_be_bytes());

    let mut mdhd = vec![0, 0, 0, 0];
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&timescale.to_be_bytes());
    mdhd.extend_from_slice(&(media_duration as u32).to_be_bytes());
    mdhd.extend_from_slice(&0x55C4u16.to_be_bytes());
    mdhd.extend_from_slice(&0u16.to_be_bytes());

    let mut hdlr = vec![0, 0, 0, 0];
    hdlr.extend_from_slice(&[0u8; 4]);
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 12]);
    hdlr.extend_from_slice(b"test\0");

    let vmhd = bx(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    let url = bx(b"url ", &[0, 0, 0, 1]);
    let mut dref = vec![0, 0, 0, 0];
    dref.extend_from_slice(&1u32.to_be_bytes());
    dref.extend_from_slice(&url);
    let dinf = bx(b"dinf", &bx(b"dref", &dref));

    // ---- サンプル表 ----
    let mut stts = vec![0, 0, 0, 0];
    stts.extend_from_slice(&1u32.to_be_bytes());
    stts.extend_from_slice(&n.to_be_bytes());
    stts.extend_from_slice(&delta.to_be_bytes());

    let mut stsc = vec![0, 0, 0, 0];
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes());

    let mut stsz = vec![0, 0, 0, 0];
    stsz.extend_from_slice(&0u32.to_be_bytes());
    stsz.extend_from_slice(&n.to_be_bytes());
    for (_, size, _) in samples {
        stsz.extend_from_slice(&size.to_be_bytes());
    }

    let mut stco = vec![0, 0, 0, 0];
    stco.extend_from_slice(&n.to_be_bytes());
    for (offset, _, _) in samples {
        stco.extend_from_slice(&(base + offset).to_be_bytes());
    }

    let keys: Vec<u32> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.2)
        .map(|(i, _)| i as u32 + 1)
        .collect();
    let mut stss = vec![0, 0, 0, 0];
    stss.extend_from_slice(&(keys.len() as u32).to_be_bytes());
    for k in &keys {
        stss.extend_from_slice(&k.to_be_bytes());
    }

    let stbl = bx(
        b"stbl",
        &[
            stsd_avc1(),
            bx(b"stts", &stts),
            bx(b"stsc", &stsc),
            bx(b"stsz", &stsz),
            bx(b"stco", &stco),
            bx(b"stss", &stss),
        ]
        .concat(),
    );
    let minf = bx(b"minf", &[vmhd, dinf, stbl].concat());
    let mdia = bx(
        b"mdia",
        &[bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), minf].concat(),
    );
    let trak = bx(b"trak", &[bx(b"tkhd", &tkhd), mdia].concat());
    bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat())
}

/// `avc1` サンプルエントリ(コーデック設定 `avcC` を含む)。
fn stsd_avc1() -> Vec<u8> {
    let mut avcc = vec![
        1,    // 設定バージョン
        0x64, // プロファイル (High)
        0x00, // 互換フラグ
        0x1F, // レベル 3.1
        0xFF, // 長さ接頭辞は 4 バイト (下位 2 ビット = 3)
        0xE1, // SPS の個数 = 1
    ];
    let sps = [0x67u8, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&sps);
    avcc.push(1); // PPS の個数
    let pps = [0x68u8, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&pps);

    let mut entry = Vec::new();
    entry.extend_from_slice(&[0u8; 6]); // 予約
    entry.extend_from_slice(&1u16.to_be_bytes()); // データ参照番号
    entry.extend_from_slice(&[0u8; 16]); // pre_defined / reserved
    entry.extend_from_slice(&1280u16.to_be_bytes());
    entry.extend_from_slice(&720u16.to_be_bytes());
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 水平解像度 72dpi
    entry.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 垂直解像度
    entry.extend_from_slice(&[0u8; 4]); // 予約
    entry.extend_from_slice(&1u16.to_be_bytes()); // フレーム数
    entry.extend_from_slice(&[0u8; 32]); // 圧縮器名
    entry.extend_from_slice(&0x0018u16.to_be_bytes()); // 深度
    entry.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined
    entry.extend_from_slice(&bx(b"avcC", &avcc));

    let mut body = vec![0, 0, 0, 0];
    body.extend_from_slice(&1u32.to_be_bytes()); // エントリ数
    body.extend_from_slice(&bx(b"avc1", &entry));
    bx(b"stsd", &body)
}

fn bx(tag: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(tag);
    out.extend_from_slice(body);
    out
}

/// 単位行列(回転なし)。
const IDENTITY: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x00, 0x00, 0x00,
];

// ---------------------------------------------------------------- 出力の確認

/// RIFF の最上位から `tag` のチャンク(または `LIST` の中身)を取り出す。
pub fn riff_top(data: &[u8], tag: &[u8; 4]) -> Option<Vec<u8>> {
    let mut pos = 12usize;
    while pos + 8 <= data.len() {
        let name: [u8; 4] = data[pos..pos + 4].try_into().ok()?;
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body = pos + 8;
        if body + size > data.len() {
            return None;
        }
        if &name == tag {
            return Some(data[body..body + size].to_vec());
        }
        if &name == b"LIST" && data.get(body..body + 4) == Some(tag.as_slice()) {
            return Some(data[body + 4..body + size].to_vec());
        }
        pos = body + size + (size & 1);
    }
    None
}

/// ISO-BMFF のボックスを名前の並びで辿って中身を取り出す。
pub fn mp4_path(data: &[u8], path: &[&[u8; 4]]) -> Option<Vec<u8>> {
    let mut current = data.to_vec();
    for tag in path {
        let mut pos = 0usize;
        let mut next = None;
        while pos + 8 <= current.len() {
            let size = u32::from_be_bytes(current[pos..pos + 4].try_into().ok()?) as usize;
            let name: [u8; 4] = current[pos + 4..pos + 8].try_into().ok()?;
            let (header, size) = if size == 1 {
                let big = u64::from_be_bytes(current.get(pos + 8..pos + 16)?.try_into().ok()?);
                (16usize, big as usize)
            } else if size == 0 {
                (8usize, current.len() - pos)
            } else {
                (8usize, size)
            };
            if size < header || pos + size > current.len() {
                return None;
            }
            if &name == *tag {
                next = Some(current[pos + header..pos + size].to_vec());
                break;
            }
            pos += size;
        }
        current = next?;
    }
    Some(current)
}

/// ビッグエンディアン u32。
pub fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

/// リトルエンディアン u32。
pub fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
