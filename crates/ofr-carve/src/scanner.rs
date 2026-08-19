//! シグネチャ走査。
//!
//! デバイスを先頭から窓ごとに読み、マジックバイトの出現位置を集める
//! (PLAN.md 5.4 の 1 段目)。窓の読み込み中に不良セクタに当たった場合は
//! その部分をゼロで埋めて先へ進む。1 セクタで走査全体を止めない。
//!
//! ファイル先頭はクラスタ境界に来るので、既定では 512 バイト境界の候補だけを
//! 採る。これで誤検出が減り、走査も速くなる。

use memchr::memmem::Finder;
use ofr_device::Device;

use crate::fill;
use crate::format::FileFormat;
use crate::signature::{SIGNATURES, Signature, max_magic_span};

/// マジックバイトが当たった 1 件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hit {
    /// 推定したファイル先頭。
    pub file_start: u64,
    /// [`SIGNATURES`] の添字。
    pub signature: usize,
}

/// 走査窓を回してシグネチャを拾う。
pub(crate) struct Scanner {
    finders: Vec<(usize, Finder<'static>)>,
    buf: Vec<u8>,
    align: u64,
    chunk: usize,
    overlap: usize,
    read_errors: u64,
}

impl Scanner {
    /// 有効な形式・整列幅・窓の大きさを決めて作る。
    pub(crate) fn new(formats: Option<&[FileFormat]>, align: u64, chunk: usize) -> Self {
        let overlap = max_magic_span().saturating_sub(1);
        let chunk = chunk.max(overlap + 64 * 1024);
        let finders = SIGNATURES
            .iter()
            .enumerate()
            .filter(|(_, s)| match formats {
                Some(list) => s.formats.iter().any(|f| list.contains(f)),
                None => true,
            })
            .map(|(i, s)| (i, Finder::new(s.magic).into_owned()))
            .collect();
        Self {
            finders,
            buf: vec![0u8; chunk],
            align: align.max(1),
            chunk,
            overlap,
            read_errors: 0,
        }
    }

    /// 有効なシグネチャが 1 つでもあるか。
    pub(crate) fn is_active(&self) -> bool {
        !self.finders.is_empty()
    }

    /// 窓の重なり幅。継ぎ目にまたがるシグネチャを拾うために次の窓と重ねる。
    pub(crate) fn overlap(&self) -> usize {
        self.overlap
    }

    /// 読み込みに失敗した回数。
    pub(crate) fn read_errors(&self) -> u64 {
        self.read_errors
    }

    /// `pos` から 1 窓ぶん走査し、見つけた候補を位置の昇順で返す。
    ///
    /// 返り値の 2 つ目は実際に読めた窓の長さ。
    pub(crate) fn scan_window(
        &mut self,
        device: &dyn Device,
        pos: u64,
        end: u64,
        out: &mut Vec<Hit>,
    ) -> usize {
        out.clear();
        let want = (end.saturating_sub(pos)).min(self.chunk as u64) as usize;
        if want == 0 {
            return 0;
        }
        let filled = self.fill(device, pos, want);
        let data = &self.buf[..filled];

        for (index, finder) in &self.finders {
            let sig: &Signature = &SIGNATURES[*index];
            for m in finder.find_iter(data) {
                let magic_at = pos + m as u64;
                let Some(file_start) = magic_at.checked_sub(sig.magic_offset) else {
                    continue;
                };
                if file_start % self.align != 0 {
                    continue;
                }
                out.push(Hit {
                    file_start,
                    signature: *index,
                });
            }
        }
        out.sort_by_key(|h| (h.file_start, h.signature));
        filled
    }

    /// 窓を埋める。読めなかった所はゼロで埋めて先へ進む。
    fn fill(&mut self, device: &dyn Device, pos: u64, want: usize) -> usize {
        let result = fill::fill(device, pos, &mut self.buf[..want], true);
        self.read_errors += result.errors;
        result.filled
    }
}
