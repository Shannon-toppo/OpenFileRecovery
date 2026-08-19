//! 壊れたバイト列で panic しないことの確認(PLAN.md 6章 5項)。
//!
//! 修復モジュールが相手にするのは定義上「壊れたファイル」なので、
//! 想定外のバイト列に強いことは機能そのものと言ってよい。ここでは
//!
//! - 乱数
//! - 各形式のヘッダだけを残して切ったもの
//! - 中身をゼロで潰したもの
//! - 長さフィールドを最大値にしたもの
//!
//! を全形式に食わせて、どれも panic せず、必ず結果かエラーが返ることを見る。

mod support;

use ofr_repair::{RepairFormat, RepairOptions, Repairer};

/// 決定的な擬似乱数(xorshift64*)。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len)
            .map(|_| {
                let mut x = self.0;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.0 = x;
                (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
            })
            .collect()
    }
}

/// 全形式に食わせて、panic しないことだけを見る。
fn try_every_format(dir: &tempfile::TempDir, name: &str, data: &[u8]) {
    let input = dir.path().join(name);
    std::fs::write(&input, data).unwrap();

    for (i, format) in [
        RepairFormat::Jpeg,
        RepairFormat::Png,
        RepairFormat::Avi,
        RepairFormat::Mp4,
    ]
    .into_iter()
    .enumerate()
    {
        let output = dir.path().join(format!("{name}.{i}.out"));
        // 結果が Ok でも Err でもよい。落ちないことと、修復元が無事なことだけを求める。
        let _ = Repairer::new(&input, &output)
            .with_options(RepairOptions {
                format: Some(format),
                // 寸法の手がかりが無いファイルでもヘッダ再構成の経路を通す。
                width: Some(64),
                height: Some(64),
                ..RepairOptions::default()
            })
            .run();
        assert_eq!(std::fs::read(&input).unwrap(), data, "修復元が変わっている");
    }
}

#[test]
fn random_data_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    for seed in [1u64, 2, 3, 4, 5] {
        let data = Rng::new(seed).bytes(128 * 1024);
        try_every_format(&dir, &format!("random-{seed}.bin"), &data);
    }
}

#[test]
fn truncated_and_zeroed_samples_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("jpg", support::jpeg(64, 48)),
        ("png", support::png(64, 48)),
        ("avi", support::avi(8)),
        ("mp4", support::mp4(8)),
    ];

    for (kind, data) in &samples {
        for cut in [0usize, 4, 16, 64, 200, 1024] {
            let head = &data[..cut.min(data.len())];
            try_every_format(&dir, &format!("{kind}-cut{cut}.bin"), head);
        }
        // ヘッダだけ本物で、中身がゼロ。
        let mut zeroed = data.clone();
        let keep = 64.min(zeroed.len());
        zeroed[keep..].fill(0);
        try_every_format(&dir, &format!("{kind}-zeroed.bin"), &zeroed);
        // 中身だけ本物で、ヘッダがゼロ。
        try_every_format(
            &dir,
            &format!("{kind}-nohead.bin"),
            &support::destroy_head(data, 512),
        );
    }
}

#[test]
fn absurd_length_fields_never_panic() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<(&str, Vec<u8>)> = vec![
        ("png", support::png(64, 48)),
        ("avi", support::avi(8)),
        ("mp4", support::mp4(8)),
    ];

    for (kind, data) in &samples {
        // 長さらしき欄を軒並み最大値にする。ボックス長・チャンク長・
        // セグメント長のどれかに当たれば、境界確認が甘い所で落ちる。
        for step in [4usize, 8, 16] {
            let mut broken = data.clone();
            for i in (0..broken.len().saturating_sub(4)).step_by(step) {
                broken[i..i + 4].copy_from_slice(&[0xFF; 4]);
            }
            try_every_format(&dir, &format!("{kind}-len{step}.bin"), &broken);
        }
    }
}
