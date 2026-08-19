# testdata

テストイメージの置き場。生成物 (`out/`, `*.img`) は gitignore する。

イメージは `crates/ofr-testfs` が Rust だけで組み立てる。OS のフォーマッタ
(`newfs_msdos` / `format`) を呼ぶ方式は CI で OS 別の分岐が要るうえ、
「削除」「クイックフォーマット」「断片化配置」を狙って作れないため採らなかった
(PLAN.md 9章の後者の案)。

```bash
# 生成 (テスト自体はこれを呼ばずにメモリ上で組み立てる)
cargo run -p ofr-testfs -- testdata/out
```

## シナリオ (ファイルシステム解析用)

| 名前 | 内容 |
|---|---|
| `fat32_deleted` / `exfat_deleted` | ファイルを配置してから一部を削除した状態 |
| `fat32_quick_format` / `exfat_quick_format` | クイックフォーマット後 (FAT 表とルートだけ消えている) |
| `fat32_fragmented` / `exfat_fragmented` | 断片化したファイルを削除した状態。連続配置の仮定が外れるケース |

## カービング用イメージ

ファイルシステムを持たない、全対応形式のサンプルをクラスタ境界に並べただけの
イメージ。カービングは FS を見ないのでこれで足りる。生成はテストコード側にある
(`crates/ofr-carve/tests/support/`)。

```bash
cargo test -p ofr-carve --test carving -- --ignored write_test_image --nocapture
```

`out/carve-test.img` と、埋めた位置の一覧 `out/carve-test.manifest.tsv`、
照合用の元ファイル `out/<名前>.bin` が出る。

```bash
ofr carve testdata/out/carve-test.img /tmp/carved --align 4096
```

## 破損サンプル集 (修復用)

`ofr repair` の回帰テストで使う、壊れたファイルの一式 (PLAN.md 9章)。正常な
JPEG / PNG / AVI / MP4 を機械生成し、そこから「ヘッダ破壊」「途中切断」
「moov 削除」「idx1 削除」「CRC 破壊」を作る。生成はテストコード側にある
(`crates/ofr-repair/tests/support/`)。

```bash
cargo test -p ofr-repair --test repair -- --ignored write_samples --nocapture
```

`out/repair/` に `healthy.*` と壊したものが並ぶ。自動テストが踏み込めない
「実際に開けるか / 再生できるか」は、ここを手元のビューアやプレイヤーで確かめる。

```bash
ofr repair testdata/out/repair/truncated.jpg /tmp/fixed.jpg
ofr repair testdata/out/repair/no-moov.mp4 /tmp/fixed.mp4 \
    --reference testdata/out/repair/healthy.mp4
```

静止画は `image` クレートで実際にエンコードした本物なので、修復結果を元の絵と
画素単位で比べられる。動画は構造だけを手で組み立てたもので、中身の画素と音声は
詰め物 (修復が見るのは索引とボックス構造だけなのでこれで足りる)。ただし
**詰め物の動画はプレイヤーで映像として再生できない**ので、上の手動確認には
実機で撮った本物の動画を使うこと。

## 生成物が本物であることの確認

生成したイメージは実際のフォーマットとして妥当なので、OS にマウントさせて
目視で確かめられる。生成側が間違っていればここで分かる。

```bash
# macOS
hdiutil attach -imagekey diskimage-class=CRawDiskImage -readonly -nobrowse testdata/out/fat32_deleted.img
ls -lR /Volumes/OFRTEST
hdiutil detach /Volumes/OFRTEST
```

```powershell
# Windows (管理者権限の PowerShell で)
Mount-DiskImage -ImagePath C:\path\to\fat32_deleted.img -StorageType Unknown
```

確認済み (2026-08): macOS 26 で FAT32 / exFAT どちらもマウントでき、削除したファイルは
見えず、長いファイル名 (`報告書 2026.txt`) とタイムスタンプが期待どおりに出ること。
