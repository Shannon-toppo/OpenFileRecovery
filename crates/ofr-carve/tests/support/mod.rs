//! テスト用のサンプルファイル生成。
//!
//! 実機のイメージは CI に置けないので、各形式の小さな正常ファイルをここで機械生成し、
//! 既知の位置に並べたテストイメージを組み立てる(PLAN.md 9章)。
//! Phase 3 の完了条件「埋めた既知ファイル群の 90% 以上を正しい境界で切り出せる」は
//! このイメージに対して測る。
//!
//! 生成物はどれも構造として正しい(サイズ・CRC・終端マーカーが整合している)が、
//! 画素データや音声データは中身のない詰め物。カービングが見るのは構造だけなので
//! これで足りる。

#![allow(dead_code)] // テスト側で使う組み合わせによって未使用になるものがある。

/// 決定的な擬似乱数。テストを再現可能にするため標準の乱数は使わない。
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        self.fill(&mut v);
        v
    }
}

fn push_u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push_u16be(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn push_u32be(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// 詰め物。0x20〜0x7F しか出さないので、偶然 0xFF(マーカーやフレーム同期の
/// 先頭バイト)を作ることがない。
fn filler(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (seed.wrapping_add(i as u8) & 0x5F) | 0x20)
        .collect()
}

// ---------------------------------------------------------------- JPEG

/// Exif 付き JPEG に入れる撮影日時。
pub const EXIF_DATETIME: &str = "2023:04:15 14:25:30";
/// Exif に入れるメーカー名。
pub const EXIF_MAKE: &str = "TestCam";
/// Exif に入れる機種名。
pub const EXIF_MODEL: &str = "OFR-1";

/// ベースライン JPEG。`exif` が真なら APP1 に Exif を入れる。
///
/// エントロピー符号にはバイトスタッフィング(`FF 00`)とリスタートマーカーを
/// 混ぜてあり、終端検出がそれらを EOI と間違えないことを確かめられる。
pub fn jpeg(width: u16, height: u16, exif: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI

    if exif {
        let payload = exif_app1(width, height);
        out.extend_from_slice(&[0xFF, 0xE1]);
        push_u16be(&mut out, (payload.len() + 2) as u16);
        out.extend_from_slice(&payload);
    } else {
        // JFIF APP0。
        out.extend_from_slice(&[0xFF, 0xE0]);
        push_u16be(&mut out, 16);
        out.extend_from_slice(b"JFIF\0");
        out.extend_from_slice(&[0x01, 0x02, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    }

    // DQT: 8 ビット精度の量子化表 1 つ。
    out.extend_from_slice(&[0xFF, 0xDB]);
    push_u16be(&mut out, 67);
    out.push(0x00);
    out.extend_from_slice(&[0x10; 64]);

    // SOF0: 8 ビット、成分 1 つ。
    out.extend_from_slice(&[0xFF, 0xC0]);
    push_u16be(&mut out, 11);
    out.push(8);
    push_u16be(&mut out, height);
    push_u16be(&mut out, width);
    out.push(1);
    out.extend_from_slice(&[0x01, 0x11, 0x00]);

    // DHT: 全部 0 個のハフマン表(構造だけ正しい)。
    out.extend_from_slice(&[0xFF, 0xC4]);
    push_u16be(&mut out, 19);
    out.push(0x00);
    out.extend_from_slice(&[0u8; 16]);

    // DRI: リスタート間隔。
    out.extend_from_slice(&[0xFF, 0xDD]);
    push_u16be(&mut out, 4);
    push_u16be(&mut out, 2);

    // SOS。
    out.extend_from_slice(&[0xFF, 0xDA]);
    push_u16be(&mut out, 8);
    out.push(1);
    out.extend_from_slice(&[0x01, 0x00]);
    out.extend_from_slice(&[0x00, 0x3F, 0x00]);

    // エントロピー符号。0xFF はスタッフィングし、リスタートマーカーも挟む。
    out.extend_from_slice(&filler(200, 0x21));
    out.extend_from_slice(&[0xFF, 0x00]);
    out.extend_from_slice(&filler(200, 0x37));
    out.extend_from_slice(&[0xFF, 0xD0]);
    out.extend_from_slice(&filler(200, 0x59));
    out.extend_from_slice(&[0xFF, 0x00, 0xFF, 0x00]);
    out.extend_from_slice(&filler(200, 0x6D));

    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    out
}

/// APP1 セグメントの中身(`Exif\0\0` + TIFF)。
fn exif_app1(width: u16, height: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"Exif\0\0");

    // ここから先のオフセットは TIFF ヘッダ先頭を 0 とする。
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II");
    push_u16le(&mut tiff, 42);
    push_u32le(&mut tiff, 8); // IFD0 の位置

    let make = format!("{EXIF_MAKE}\0");
    let model = format!("{EXIF_MODEL}\0");
    let datetime = format!("{EXIF_DATETIME}\0");

    // IFD0(4 エントリ)+ 次 IFD へのポインタ = 2 + 4*12 + 4 = 54 バイト。
    let ifd0_at = 8u32;
    let ifd0_len = 2 + 4 * 12 + 4;
    let exif_ifd_at = ifd0_at + ifd0_len;
    // ExifIFD(3 エントリ)= 2 + 3*12 + 4 = 42 バイト。
    let exif_ifd_len = 2 + 3 * 12 + 4;
    let data_at = exif_ifd_at + exif_ifd_len;
    let make_at = data_at;
    let model_at = make_at + make.len() as u32;
    let datetime_at = model_at + model.len() as u32;

    push_u16le(&mut tiff, 4);
    push_entry(&mut tiff, 0x010F, 2, make.len() as u32, make_at);
    push_entry(&mut tiff, 0x0110, 2, model.len() as u32, model_at);
    push_entry(&mut tiff, 0x0112, 3, 1, 1); // Orientation = 1
    push_entry(&mut tiff, 0x8769, 4, 1, exif_ifd_at);
    push_u32le(&mut tiff, 0); // 次の IFD なし

    push_u16le(&mut tiff, 3);
    push_entry(&mut tiff, 0x9003, 2, datetime.len() as u32, datetime_at);
    push_entry(&mut tiff, 0xA002, 4, 1, u32::from(width));
    push_entry(&mut tiff, 0xA003, 4, 1, u32::from(height));
    push_u32le(&mut tiff, 0);

    tiff.extend_from_slice(make.as_bytes());
    tiff.extend_from_slice(model.as_bytes());
    tiff.extend_from_slice(datetime.as_bytes());

    out.extend_from_slice(&tiff);
    out
}

/// IFD エントリ 1 つ。値が 4 バイトに収まる型はそのまま、収まらない ASCII は
/// TIFF 先頭からのオフセットを入れる。
fn push_entry(out: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32) {
    push_u16le(out, tag);
    push_u16le(out, ty);
    push_u32le(out, count);
    if ty == 2 && count > 4 {
        push_u32le(out, value);
    } else if ty == 3 {
        push_u16le(out, value as u16);
        push_u16le(out, 0);
    } else {
        push_u32le(out, value);
    }
}

// ---------------------------------------------------------------- PNG

/// PNG。IHDR / IDAT / IEND の 3 チャンクで、CRC も正しく入れる。
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::new();
    push_u32be(&mut ihdr, width);
    push_u32be(&mut ihdr, height);
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &filler(512, 0x11));
    push_chunk(&mut out, b"IEND", &[]);
    out
}

fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    push_u32be(out, data.len() as u32);
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(tag);
    hasher.update(data);
    push_u32be(out, hasher.finalize());
}

// ---------------------------------------------------------------- GIF

/// GIF89a。大域カラーテーブル + 画像 1 枚 + トレーラ。
pub fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"GIF89a");
    push_u16le(&mut out, width);
    push_u16le(&mut out, height);
    out.push(0xF0); // 大域カラーテーブルあり、サイズ 2 色
    out.push(0); // 背景色
    out.push(0); // アスペクト比
    out.extend_from_slice(&[0, 0, 0, 0xFF, 0xFF, 0xFF]); // カラーテーブル

    // グラフィック制御拡張。
    out.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // 画像記述子。
    out.push(0x2C);
    push_u16le(&mut out, 0);
    push_u16le(&mut out, 0);
    push_u16le(&mut out, width);
    push_u16le(&mut out, height);
    out.push(0); // 局所カラーテーブルなし
    out.push(2); // LZW 最小符号長
    for _ in 0..3 {
        out.push(64);
        out.extend_from_slice(&filler(64, 0x05));
    }
    out.push(0); // サブブロック終端
    out.push(0x3B); // トレーラ
    out
}

// ---------------------------------------------------------------- RIFF

/// WAV。fmt + data の最小構成。
pub fn wav(samples: usize) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"WAVE");

    body.extend_from_slice(b"fmt ");
    push_u32le(&mut body, 16);
    push_u16le(&mut body, 1); // PCM
    push_u16le(&mut body, 2); // 2ch
    push_u32le(&mut body, 44100);
    push_u32le(&mut body, 176_400); // バイト/秒
    push_u16le(&mut body, 4);
    push_u16le(&mut body, 16);

    let data = filler(samples * 4, 0x31);
    body.extend_from_slice(b"data");
    push_u32le(&mut body, data.len() as u32);
    body.extend_from_slice(&data);

    riff(&body)
}

/// AVI。hdrl(avih)+ movi の最小構成。
pub fn avi(width: u32, height: u32, frames: u32) -> Vec<u8> {
    let mut avih = Vec::new();
    push_u32le(&mut avih, 33_333); // マイクロ秒/フレーム (30fps)
    push_u32le(&mut avih, 1_000_000);
    push_u32le(&mut avih, 0);
    push_u32le(&mut avih, 0x10);
    push_u32le(&mut avih, frames);
    push_u32le(&mut avih, 0);
    push_u32le(&mut avih, 1);
    push_u32le(&mut avih, 0);
    push_u32le(&mut avih, width);
    push_u32le(&mut avih, height);
    avih.extend_from_slice(&[0u8; 16]);
    assert_eq!(avih.len(), 56);

    let mut hdrl = Vec::new();
    hdrl.extend_from_slice(b"hdrl");
    hdrl.extend_from_slice(b"avih");
    push_u32le(&mut hdrl, avih.len() as u32);
    hdrl.extend_from_slice(&avih);

    let mut movi = Vec::new();
    movi.extend_from_slice(b"movi");
    for i in 0..frames {
        let data = filler(256, i as u8);
        movi.extend_from_slice(b"00dc");
        push_u32le(&mut movi, data.len() as u32);
        movi.extend_from_slice(&data);
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"AVI ");
    body.extend_from_slice(b"LIST");
    push_u32le(&mut body, hdrl.len() as u32);
    body.extend_from_slice(&hdrl);
    body.extend_from_slice(b"LIST");
    push_u32le(&mut body, movi.len() as u32);
    body.extend_from_slice(&movi);

    riff(&body)
}

fn riff(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    push_u32le(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    out
}

// ---------------------------------------------------------------- ISO-BMFF

/// ISO-BMFF の `mvhd` に入れる作成日時(1904 起点の秒)。2023-04-15 14:25:30 UTC。
pub const MVHD_CREATED: u32 = 3_764_413_530;

/// MP4(ブランド `isom`)。ftyp + moov(mvhd)+ mdat。
pub fn mp4(payload: usize) -> Vec<u8> {
    isobmff(b"isom", &[b"isom", b"mp41"], payload, false)
}

/// QuickTime MOV(ブランド `qt  `)。
pub fn mov(payload: usize) -> Vec<u8> {
    isobmff(b"qt  ", &[b"qt  "], payload, false)
}

/// HEIC(ブランド `heic`)。meta ボックスを持つ。
pub fn heic(payload: usize) -> Vec<u8> {
    isobmff(b"heic", &[b"mif1", b"heic"], payload, true)
}

fn isobmff(brand: &[u8; 4], compatible: &[&[u8; 4]], payload: usize, meta: bool) -> Vec<u8> {
    let mut out = Vec::new();

    let ftyp_size = 16 + 4 * compatible.len();
    push_u32be(&mut out, ftyp_size as u32);
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(brand);
    push_u32be(&mut out, 0); // マイナーバージョン
    for c in compatible {
        out.extend_from_slice(*c);
    }

    if meta {
        let body = filler(24, 0x71);
        push_u32be(&mut out, (8 + body.len()) as u32);
        out.extend_from_slice(b"meta");
        out.extend_from_slice(&body);
    }

    // moov > mvhd(バージョン 0)。
    let mvhd = mvhd_v0();
    push_u32be(&mut out, (8 + mvhd.len()) as u32);
    out.extend_from_slice(b"moov");
    out.extend_from_slice(&mvhd);

    let data = filler(payload, 0x4D);
    push_u32be(&mut out, (8 + data.len()) as u32);
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(&data);
    out
}

fn mvhd_v0() -> Vec<u8> {
    let mut b = Vec::new();
    push_u32be(&mut b, 108);
    b.extend_from_slice(b"mvhd");
    push_u32be(&mut b, 0); // バージョン 0 + フラグ
    push_u32be(&mut b, MVHD_CREATED);
    push_u32be(&mut b, MVHD_CREATED);
    push_u32be(&mut b, 1000); // タイムスケール
    push_u32be(&mut b, 5000); // 長さ = 5 秒
    push_u32be(&mut b, 0x0001_0000); // レート
    push_u16be(&mut b, 0x0100); // 音量
    b.extend_from_slice(&[0u8; 10]);
    b.extend_from_slice(&[0u8; 36]); // 行列
    b.extend_from_slice(&[0u8; 24]); // 予約
    push_u32be(&mut b, 2); // 次のトラック ID
    assert_eq!(b.len(), 108);
    b
}

// ---------------------------------------------------------------- MP3

/// MP3。ID3v2 タグ + MPEG1 Layer3 フレーム + ID3v1 タグ。
pub fn mp3(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();

    // ID3v2.3 ヘッダ。サイズは synchsafe integer。
    let tag = filler(100, 0x49);
    out.extend_from_slice(b"ID3");
    out.extend_from_slice(&[0x03, 0x00, 0x00]);
    let size = tag.len() as u32;
    out.extend_from_slice(&[
        ((size >> 21) & 0x7F) as u8,
        ((size >> 14) & 0x7F) as u8,
        ((size >> 7) & 0x7F) as u8,
        (size & 0x7F) as u8,
    ]);
    out.extend_from_slice(&tag);

    // 128kbps / 44.1kHz / パディングなし → 417 バイト固定。
    for _ in 0..frames {
        out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        out.extend_from_slice(&filler(413, 0x55));
    }

    // ID3v1(128 バイト固定)。
    out.extend_from_slice(b"TAG");
    out.extend_from_slice(&filler(125, 0x61));
    out
}

// ---------------------------------------------------------------- ZIP

/// 無圧縮 ZIP。`entries` は (パス名, 中身)。
pub fn zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();

    for (name, data) in entries {
        let offset = out.len() as u32;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(data);
        let crc = hasher.finalize();

        out.extend_from_slice(b"PK\x03\x04");
        push_u16le(&mut out, 20); // 展開に必要なバージョン
        push_u16le(&mut out, 0); // 汎用フラグ
        push_u16le(&mut out, 0); // 無圧縮
        push_u16le(&mut out, 0); // 時刻
        push_u16le(&mut out, 0x2E8F); // 日付 (2023-04-15)
        push_u32le(&mut out, crc);
        push_u32le(&mut out, data.len() as u32);
        push_u32le(&mut out, data.len() as u32);
        push_u16le(&mut out, name.len() as u16);
        push_u16le(&mut out, 0); // 拡張フィールドなし
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(b"PK\x01\x02");
        push_u16le(&mut central, 20); // 作成バージョン
        push_u16le(&mut central, 20);
        push_u16le(&mut central, 0);
        push_u16le(&mut central, 0);
        push_u16le(&mut central, 0);
        push_u16le(&mut central, 0x2E8F);
        push_u32le(&mut central, crc);
        push_u32le(&mut central, data.len() as u32);
        push_u32le(&mut central, data.len() as u32);
        push_u16le(&mut central, name.len() as u16);
        push_u16le(&mut central, 0);
        push_u16le(&mut central, 0); // コメント
        push_u16le(&mut central, 0); // ディスク番号
        push_u16le(&mut central, 0); // 内部属性
        push_u32le(&mut central, 0); // 外部属性
        push_u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }

    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);

    out.extend_from_slice(b"PK\x05\x06");
    push_u16le(&mut out, 0);
    push_u16le(&mut out, 0);
    push_u16le(&mut out, entries.len() as u16);
    push_u16le(&mut out, entries.len() as u16);
    push_u32le(&mut out, central.len() as u32);
    push_u32le(&mut out, cd_offset);
    push_u16le(&mut out, 0); // コメントなし
    out
}

/// docx(OOXML)。中身は最小限だが、拡張子判定に必要なパス名は本物と同じ。
pub fn docx() -> Vec<u8> {
    zip(&[
        (
            "[Content_Types].xml",
            b"<?xml version=\"1.0\"?><Types/>".to_vec(),
        ),
        (
            "_rels/.rels",
            b"<?xml version=\"1.0\"?><Relationships/>".to_vec(),
        ),
        (
            "word/document.xml",
            b"<?xml version=\"1.0\"?><w:document/>".to_vec(),
        ),
    ])
}

// ---------------------------------------------------------------- PDF

/// PDF。オブジェクト 3 つ + xref + `%%EOF`。
pub fn pdf() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count 0 >>\nendobj\n");
    out.extend_from_slice(b"3 0 obj\n<< /CreationDate (D:20230415142530+09'00') >>\nendobj\n");
    let xref_at = out.len();
    out.extend_from_slice(b"xref\n0 4\n");
    out.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    out.extend_from_slice(format!("startxref\n{xref_at}\n").as_bytes());
    out.extend_from_slice(b"%%EOF\n");
    out
}
