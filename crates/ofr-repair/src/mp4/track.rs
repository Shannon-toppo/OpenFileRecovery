//! `moov` の読み取りと組み立て。
//!
//! 修復の出力は `moov` を必ず組み直す。壊れた表を部分的に書き換えるより、
//! 読めた情報から一貫した表を作り直す方が確実で、結果も読みやすい。
//!
//! 組み直しで捨ててしまうもの(章立て、`udta` の付随情報など)はあるが、
//! 再生に要るもの — コーデック設定(`stsd`)、時間軸、表示行列(回転) — は
//! 元の値をそのまま引き継ぐ。縦向きで撮った動画が横に倒れて出てくるのを
//! 避けるため、行列だけは特に丁寧に写している。

use super::boxes::{self, be32, be64};

/// 表として持つサンプル数の上限。壊れた数値で無限にメモリを食わないための歯止め。
///
/// 200 万サンプルは 30fps で約 18 時間ぶん。これを超える値は壊れているとみなす。
const MAX_SAMPLES: usize = 2_000_000;

/// 単位行列(回転なし)。
const IDENTITY_MATRIX: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x00, 0x00, 0x00,
];

/// 1 サンプル(映像なら 1 フレーム)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sample {
    /// ファイル先頭からの位置。
    pub offset: u64,
    /// バイト数。
    pub size: u32,
    /// 再生時間(メディアのタイムスケール)。
    pub duration: u32,
    /// キーフレームか。
    pub sync: bool,
}

/// トラック 1 本。
#[derive(Debug, Clone)]
pub(crate) struct Track {
    /// トラック ID(1 から)。
    pub id: u32,
    /// `vide` / `soun` など。
    pub handler: [u8; 4],
    /// メディアのタイムスケール(1 秒あたりの目盛り数)。
    pub timescale: u32,
    /// 表示行列。回転情報がここに入る。
    pub matrix: [u8; 36],
    /// 表示幅(16.16 固定小数)。
    pub width: u32,
    /// 表示高さ(16.16 固定小数)。
    pub height: u32,
    /// `stsd` ボックス全体。コーデック設定(avcC など)がここに入っている。
    pub stsd: Vec<u8>,
    /// `vmhd` / `smhd` ボックス全体。
    pub media_header: Vec<u8>,
    /// サンプル表。
    pub samples: Vec<Sample>,
}

impl Track {
    /// 映像トラックか。
    pub(crate) fn is_video(&self) -> bool {
        &self.handler == b"vide"
    }

    /// メディアのタイムスケールでの総再生時間。
    pub(crate) fn duration(&self) -> u64 {
        self.samples.iter().map(|s| u64::from(s.duration)).sum()
    }

    /// サンプル 1 つあたりの平均再生時間。参照ファイルから借りるときに使う。
    pub(crate) fn average_duration(&self) -> u32 {
        if self.samples.is_empty() {
            return self.timescale.max(1) / 30; // 30fps 相当を仮置き
        }
        (self.duration() / self.samples.len() as u64).max(1) as u32
    }
}

/// `moov` の中身からトラックを読み取る。
pub(crate) fn parse_tracks(moov: &[u8]) -> Vec<Track> {
    boxes::walk(moov)
        .filter(|b| &b.tag == b"trak")
        .filter_map(|b| parse_track(b.body))
        .collect()
}

fn parse_track(trak: &[u8]) -> Option<Track> {
    let tkhd = boxes::find(trak, b"tkhd")?;
    let mdia = boxes::find(trak, b"mdia")?;
    let mdhd = boxes::find(mdia, b"mdhd")?;
    let hdlr = boxes::find(mdia, b"hdlr")?;
    let minf = boxes::find(mdia, b"minf")?;
    let stbl = boxes::find(minf, b"stbl")?;

    // tkhd はバージョンで欄の位置が変わる(v1 は日時と長さが 64 ビット)。
    // v0: 版/フラグ4 + 日時8 + ID4 + 予約4 + 長さ4 + 予約8 + レイヤ等8 → 行列は 40。
    // v1: 日時が 16、長さが 8 になるぶん 12 バイト後ろへずれる。
    let version = *tkhd.first()?;
    let (id, matrix_at) = if version == 1 {
        (be32(tkhd, 20), 52)
    } else {
        (be32(tkhd, 12), 40)
    };
    let mut matrix = IDENTITY_MATRIX;
    if let Some(m) = tkhd.get(matrix_at..matrix_at + 36) {
        matrix.copy_from_slice(m);
    }
    let width = be32(tkhd, matrix_at + 36);
    let height = be32(tkhd, matrix_at + 40);

    let timescale = if *mdhd.first()? == 1 {
        be32(mdhd, 20)
    } else {
        be32(mdhd, 12)
    };

    let handler: [u8; 4] = hdlr.get(8..12)?.try_into().ok()?;
    let media_header = boxes::find_whole(minf, b"vmhd")
        .or_else(|| boxes::find_whole(minf, b"smhd"))
        .map(<[u8]>::to_vec)
        .unwrap_or_else(|| default_media_header(&handler));

    Some(Track {
        id: id.max(1),
        handler,
        timescale: timescale.max(1),
        matrix,
        width,
        height,
        stsd: boxes::find_whole(stbl, b"stsd")?.to_vec(),
        media_header,
        samples: parse_samples(stbl),
    })
}

/// `vmhd` / `smhd` が見当たらないときの最小構成。
fn default_media_header(handler: &[u8; 4]) -> Vec<u8> {
    if handler == b"vide" {
        // version/flags = 1 (仕様上 flags は 1 固定)、graphicsmode + opcolor。
        boxes::make(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
    } else {
        boxes::make(b"smhd", &[0, 0, 0, 0, 0, 0, 0, 0])
    }
}

/// `stbl` のサンプル表を、サンプル 1 件ずつの一覧に展開する。
///
/// MP4 のサンプル表は「チャンクの位置」「チャンクあたりのサンプル数」
/// 「サンプルの大きさ」に分かれていて、位置を出すには 3 つを突き合わせる必要がある。
pub(crate) fn parse_samples(stbl: &[u8]) -> Vec<Sample> {
    let Some(stsz) = boxes::find(stbl, b"stsz") else {
        return Vec::new();
    };
    let uniform = be32(stsz, 4);
    let mut count = (be32(stsz, 8) as usize).min(MAX_SAMPLES);
    // 大きさの表があるなら、そこに実在する分しか信じない。壊れた件数で
    // 巨大な表を組み立てないための歯止め。
    if uniform == 0 {
        count = count.min(stsz.len().saturating_sub(12) / 4);
    }
    if count == 0 {
        return Vec::new();
    }
    let sizes: Vec<u32> = if uniform > 0 {
        vec![uniform; count]
    } else {
        (0..count).map(|i| be32(stsz, 12 + i * 4)).collect()
    };

    // チャンクの位置。co64 は 64 ビット版。
    let chunks: Vec<u64> = match (boxes::find(stbl, b"stco"), boxes::find(stbl, b"co64")) {
        (Some(stco), _) => {
            let n = (be32(stco, 4) as usize).min(stco.len().saturating_sub(8) / 4);
            (0..n).map(|i| u64::from(be32(stco, 8 + i * 4))).collect()
        }
        (None, Some(co64)) => {
            let n = (be32(co64, 4) as usize).min(co64.len().saturating_sub(8) / 8);
            (0..n).map(|i| be64(co64, 8 + i * 8)).collect()
        }
        (None, None) => return Vec::new(),
    };

    // チャンクごとのサンプル数(区間で表現されている)。
    let stsc = boxes::find(stbl, b"stsc").unwrap_or(&[]);
    let runs = (be32(stsc, 4) as usize).min(stsc.len().saturating_sub(8) / 12);
    let per_chunk = |chunk_index: usize| -> u32 {
        let mut samples = 1u32;
        for r in 0..runs {
            let first = be32(stsc, 8 + r * 12) as usize;
            if first == 0 || first > chunk_index + 1 {
                break;
            }
            samples = be32(stsc, 8 + r * 12 + 4).max(1);
        }
        samples
    };

    // サンプルごとの再生時間。
    let stts = boxes::find(stbl, b"stts").unwrap_or(&[]);
    let stts_runs = (be32(stts, 4) as usize).min(stts.len().saturating_sub(8) / 8);
    let mut durations = Vec::with_capacity(count);
    for r in 0..stts_runs {
        let n = be32(stts, 8 + r * 8) as usize;
        let delta = be32(stts, 8 + r * 8 + 4);
        for _ in 0..n.min(count.saturating_sub(durations.len())) {
            durations.push(delta);
        }
    }
    durations.resize(count, *durations.last().unwrap_or(&0));

    // キーフレーム。stss が無ければ全部キーフレーム。
    let sync: Vec<bool> = match boxes::find(stbl, b"stss") {
        Some(stss) => {
            let n = (be32(stss, 4) as usize).min(stss.len().saturating_sub(8) / 4);
            let mut flags = vec![false; count];
            for i in 0..n {
                let number = be32(stss, 8 + i * 4) as usize;
                if (1..=count).contains(&number) {
                    flags[number - 1] = true;
                }
            }
            flags
        }
        None => vec![true; count],
    };

    // チャンクを順に辿り、サンプルの位置を積み上げる。
    let mut out = Vec::with_capacity(count);
    let mut index = 0usize;
    for (ci, &chunk_offset) in chunks.iter().enumerate() {
        let mut at = chunk_offset;
        for _ in 0..per_chunk(ci) {
            if index >= count {
                return out;
            }
            out.push(Sample {
                offset: at,
                size: sizes[index],
                duration: durations[index],
                sync: sync[index],
            });
            at = at.saturating_add(u64::from(sizes[index]));
            index += 1;
        }
    }
    out
}

/// トラックから `moov` を組み立てる。
///
/// `movie_timescale` は動画全体の時間の目盛り。1000 (ミリ秒) にしておく。
pub(crate) fn build_moov(tracks: &[Track], movie_timescale: u32) -> Vec<u8> {
    let movie_duration = tracks
        .iter()
        .map(|t| t.duration() * u64::from(movie_timescale) / u64::from(t.timescale.max(1)))
        .max()
        .unwrap_or(0);
    let next_track = tracks.iter().map(|t| t.id).max().unwrap_or(0) + 1;

    let mut children = vec![mvhd(movie_timescale, movie_duration, next_track)];
    for t in tracks {
        children.push(trak(t, movie_timescale));
    }
    boxes::container(b"moov", &children)
}

fn mvhd(timescale: u32, duration: u64, next_track: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(100);
    b.extend_from_slice(&[0, 0, 0, 0]); // バージョン 0 + フラグ
    b.extend_from_slice(&0u32.to_be_bytes()); // 作成日時 (不明)
    b.extend_from_slice(&0u32.to_be_bytes()); // 更新日時 (不明)
    b.extend_from_slice(&timescale.to_be_bytes());
    b.extend_from_slice(&(duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    b.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // 再生速度 1.0
    b.extend_from_slice(&0x0100u16.to_be_bytes()); // 音量 1.0
    b.extend_from_slice(&[0u8; 2]); // 予約
    b.extend_from_slice(&[0u8; 8]); // 予約
    b.extend_from_slice(&IDENTITY_MATRIX);
    b.extend_from_slice(&[0u8; 24]); // pre_defined
    b.extend_from_slice(&next_track.to_be_bytes());
    boxes::make(b"mvhd", &b)
}

fn trak(t: &Track, movie_timescale: u32) -> Vec<u8> {
    let media_duration = t.duration();
    let track_duration =
        media_duration * u64::from(movie_timescale) / u64::from(t.timescale.max(1));

    let mut tkhd = Vec::with_capacity(84);
    // フラグ 7 = 有効 + 動画に使う + プレビューに使う。
    tkhd.extend_from_slice(&[0, 0, 0, 7]);
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // 作成日時
    tkhd.extend_from_slice(&0u32.to_be_bytes()); // 更新日時
    tkhd.extend_from_slice(&t.id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 4]); // 予約
    tkhd.extend_from_slice(&(track_duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 8]); // 予約
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // レイヤー
    tkhd.extend_from_slice(&0u16.to_be_bytes()); // 代替グループ
    let volume: u16 = if t.is_video() { 0 } else { 0x0100 };
    tkhd.extend_from_slice(&volume.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 2]); // 予約
    tkhd.extend_from_slice(&t.matrix);
    tkhd.extend_from_slice(&t.width.to_be_bytes());
    tkhd.extend_from_slice(&t.height.to_be_bytes());

    let mut mdhd = Vec::with_capacity(24);
    mdhd.extend_from_slice(&[0, 0, 0, 0]);
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&t.timescale.to_be_bytes());
    mdhd.extend_from_slice(&(media_duration.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    mdhd.extend_from_slice(&0x55C4u16.to_be_bytes()); // 言語 = und
    mdhd.extend_from_slice(&0u16.to_be_bytes());

    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0, 0, 0, 0]);
    hdlr.extend_from_slice(&[0u8; 4]); // pre_defined
    hdlr.extend_from_slice(&t.handler);
    hdlr.extend_from_slice(&[0u8; 12]); // 予約
    hdlr.extend_from_slice(b"ofr-repair\0");

    let minf = boxes::container(b"minf", &[t.media_header.clone(), dinf(), stbl(t)]);
    let mdia = boxes::container(
        b"mdia",
        &[
            boxes::make(b"mdhd", &mdhd),
            boxes::make(b"hdlr", &hdlr),
            minf,
        ],
    );
    boxes::container(b"trak", &[boxes::make(b"tkhd", &tkhd), mdia])
}

/// データが同じファイルの中にあることを示す最小の `dinf`。
fn dinf() -> Vec<u8> {
    let url = boxes::make(b"url ", &[0, 0, 0, 1]); // フラグ 1 = 自分自身の中
    let mut dref = vec![0, 0, 0, 0];
    dref.extend_from_slice(&1u32.to_be_bytes());
    dref.extend_from_slice(&url);
    boxes::container(b"dinf", &[boxes::make(b"dref", &dref)])
}

/// サンプル表を組み立てる。
///
/// 「1 チャンク = 1 サンプル」にして `co64` に実位置を並べる。表は少し大きくなるが、
/// チャンクの組み方に依存しないので、壊れたファイルから作り直すときに誤りが入らない。
fn stbl(t: &Track) -> Vec<u8> {
    let n = t.samples.len() as u32;

    // stts: 同じ長さが続く区間にまとめる。
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for s in &t.samples {
        match runs.last_mut() {
            Some((count, delta)) if *delta == s.duration => *count += 1,
            _ => runs.push((1, s.duration)),
        }
    }
    let mut stts = vec![0, 0, 0, 0];
    stts.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, delta) in &runs {
        stts.extend_from_slice(&count.to_be_bytes());
        stts.extend_from_slice(&delta.to_be_bytes());
    }

    // stsc: 全チャンクが 1 サンプル。
    let mut stsc = vec![0, 0, 0, 0];
    stsc.extend_from_slice(&1u32.to_be_bytes());
    stsc.extend_from_slice(&1u32.to_be_bytes()); // 最初のチャンク
    stsc.extend_from_slice(&1u32.to_be_bytes()); // チャンクあたり 1 サンプル
    stsc.extend_from_slice(&1u32.to_be_bytes()); // 記述番号

    let mut stsz = vec![0, 0, 0, 0];
    stsz.extend_from_slice(&0u32.to_be_bytes()); // 一律サイズではない
    stsz.extend_from_slice(&n.to_be_bytes());
    for s in &t.samples {
        stsz.extend_from_slice(&s.size.to_be_bytes());
    }

    let mut co64 = vec![0, 0, 0, 0];
    co64.extend_from_slice(&n.to_be_bytes());
    for s in &t.samples {
        co64.extend_from_slice(&s.offset.to_be_bytes());
    }

    let mut children = vec![
        t.stsd.clone(),
        boxes::make(b"stts", &stts),
        boxes::make(b"stsc", &stsc),
        boxes::make(b"stsz", &stsz),
        boxes::make(b"co64", &co64),
    ];

    // 全部キーフレームなら stss は不要(無い方が「全部キーフレーム」の意味になる)。
    if t.samples.iter().any(|s| !s.sync) {
        let keys: Vec<u32> = t
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.sync)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        let mut stss = vec![0, 0, 0, 0];
        stss.extend_from_slice(&(keys.len() as u32).to_be_bytes());
        for k in keys {
            stss.extend_from_slice(&k.to_be_bytes());
        }
        children.push(boxes::make(b"stss", &stss));
    }

    boxes::container(b"stbl", &children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(samples: Vec<Sample>) -> Track {
        Track {
            id: 1,
            handler: *b"vide",
            timescale: 30000,
            matrix: IDENTITY_MATRIX,
            width: 1920 << 16,
            height: 1080 << 16,
            stsd: boxes::make(b"stsd", &[0u8; 8]),
            media_header: boxes::make(b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]),
            samples,
        }
    }

    fn samples(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| Sample {
                offset: 1000 + i as u64 * 500,
                size: 500,
                duration: 1001,
                sync: i % 10 == 0,
            })
            .collect()
    }

    #[test]
    fn built_moov_reads_back_identically() {
        let original = track(samples(25));
        let moov = build_moov(std::slice::from_ref(&original), 1000);

        let body = boxes::find(&moov, b"moov").unwrap();
        let back = parse_tracks(body);
        assert_eq!(back.len(), 1);
        let back = &back[0];

        assert_eq!(back.id, original.id);
        assert_eq!(back.handler, *b"vide");
        assert_eq!(back.timescale, 30000);
        assert_eq!(back.width, original.width);
        assert_eq!(back.samples, original.samples);
    }

    #[test]
    fn all_sync_tracks_omit_the_sync_table() {
        let mut s = samples(4);
        for x in &mut s {
            x.sync = true;
        }
        let moov = build_moov(&[track(s)], 1000);
        let body = boxes::find(&moov, b"moov").unwrap();
        let trak = boxes::find(body, b"trak").unwrap();
        let stbl = boxes::find(
            boxes::find(boxes::find(trak, b"mdia").unwrap(), b"minf").unwrap(),
            b"stbl",
        )
        .unwrap();
        assert!(boxes::find(stbl, b"stss").is_none());
        // stss が無いので読み戻すと全部キーフレーム。
        assert!(parse_samples(stbl).iter().all(|s| s.sync));
    }

    #[test]
    fn stts_runs_are_merged() {
        let moov = build_moov(&[track(samples(100))], 1000);
        let body = boxes::find(&moov, b"moov").unwrap();
        let trak = boxes::find(body, b"trak").unwrap();
        let stbl = boxes::find(
            boxes::find(boxes::find(trak, b"mdia").unwrap(), b"minf").unwrap(),
            b"stbl",
        )
        .unwrap();
        let stts = boxes::find(stbl, b"stts").unwrap();
        // 全部同じ長さなので区間は 1 つ。
        assert_eq!(be32(stts, 4), 1);
        assert_eq!(be32(stts, 8), 100);
    }
}
