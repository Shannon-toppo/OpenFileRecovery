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
