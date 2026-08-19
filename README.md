# Open File Recovery

USB メモリと SD カードを主対象にしたデータ復旧ツール。Rust 製のコアと CLI、
のちに Tauri の GUI を載せる。設計と全体計画は [PLAN.md](PLAN.md) を参照。

MIT ライセンス。GPL/LGPL のコードは流用していない。外部バイナリ (ffmpeg など) にも依存しない。

## いまできること (Phase 2)

| コマンド | 内容 |
|---|---|
| `ofr list` | 接続されているデバイスの一覧。起動ディスクは選択不可として表示する |
| `ofr image` | 壊れかけメディアの吸い出し (ddrescue 方式の多段パス + mapfile による中断・再開) |
| `ofr scan` | FAT32 / exFAT の解析。生きているファイル、削除されたファイル、フォーマットで消えたフォルダを一覧する |
| `ofr restore` | 見つかったファイルをフォルダ構造ごと復元先へ書き出す |

カービング (Phase 3)、コピー (Phase 4)、修復 (Phase 5)、GUI (Phase 6) は未実装。

## ビルドと実行

```bash
cargo build --release
```

生成物は `target/release/ofr`。生デバイスの読み込みには管理者 / root 権限が必要。

```bash
# デバイスを探す (権限なしで動く)
ofr list

# 吸い出す。出力先は必ず別のディスクにすること
sudo ofr image /dev/disk4 /Volumes/Backup/usb.img
```

Windows では管理者コマンドプロンプトから `ofr image \\.\PhysicalDrive2 D:\usb.img` のように使う。

```bash
# 吸い出したイメージを解析する (デバイスを直接指定してもよい)
ofr scan /Volumes/Backup/usb.img

# 見つかったものを復元する。復元先は必ず別のディスクにすること
ofr restore /Volumes/Backup/usb.img /Volumes/Backup/recovered
```

`ofr scan` は 2 段構えで探す。まずディレクトリツリーを普通に辿り、次にデータ領域を
全部舐めてルートから辿れなくなったフォルダを拾う。クイックフォーマットは FAT 表と
ルートしか消さないので、**フォーマット後の復元は後者が主力**になる。
拾ったフォルダは名前が分からない (名前は親フォルダ側にあるため) ので、
`Lost+Found/dir_00000003/` のような仮の名前でツリーに出す。

### `ofr image` の主なオプション

| オプション | 既定 | 内容 |
|---|---|---|
| `-m, --mapfile <PATH>` | `<出力>.map` | 取得済み/不良領域の記録。GNU ddrescue と互換 |
| `-r, --retries <N>` | 3 | 不良セクタのリトライ回数 (指数バックオフ付き) |
| `-b, --block-size <SIZE>` | `1M` | コピーパスの読み込み単位。エラー多発時は自動で縮小する |
| `--no-trim` / `--no-scrape` / `--no-retry` | — | 各パスを省く |
| `--unmount` | — | 開始前に `diskutil unmountDisk` する (macOS) |

中断 (Ctrl-C) しても mapfile が残るので、同じコマンドを実行すれば続きから再開する。
終了コードは 0 = 全域取得、1 = 未取得領域あり / 中断、2 = エラー。

### `ofr scan` / `ofr restore` の主なオプション

| オプション | 内容 |
|---|---|
| `--fs auto\|fat32\|exfat` | ファイルシステムの指定。既定は自動判定 |
| `--offset <SIZE>` | ボリュームの開始位置を直接指定する (パーティションテーブルが壊れている場合) |
| `--include <PATTERN>` | 名前かパスで絞る (`--include '*.jpg'`)。複数指定できる |
| `--status <LIST>` | 状態で絞る (`--status deleted,orphaned`) |
| `--no-orphans` | 全クラスタ走査を省く。速いが、フォーマット後のデバイスでは何も出ない |
| `--no-deleted` | 削除済みを探さない |
| `--tree` / `--json` | 表示形式 (`ofr scan`) |
| `--dry-run` / `--flatten` | 書き出さずに確認 / 階層を作らず平らに並べる (`ofr restore`) |

復元した項目には、信用してよい度合いを注記として出す。

- **連続配置と仮定**: FAT チェーンが失われているので、開始位置から連続していると
  仮定して回収した。断片化していたファイルはこの仮定で壊れる (Phase 5 の修復行き)
- **使用中クラスタ N 個**: その領域は今も別のファイルに使われている。上書きされている可能性が高い
- **名前が不完全**: FAT32 で長い名前 (LFN) が残っていない削除ファイル。8.3 名の先頭 1 文字は
  仕様上どうやっても戻らないので `_` で埋めてある (exFAT では起きない)

終了コードは 0 = 全部できた、1 = 何も見つからない / 一部だけ / 中断、2 = エラー。

## 安全のための決まり

- **復旧元デバイスには一切書き込まない。** デバイス抽象 (`trait Device`) に書き込み API を作っていない。
- OS の起動ディスクは復旧元として選べない。
- 出力先が復旧元と同じデバイス上にある場合はエラーにする。
- 壊れかけたメディアは、アクセスのたびに劣化しうる。**まずイメージを取り、以後はイメージを解析すること。**

## 対象外

- **物理障害**: コントローラが認識しない、OS からブロックデバイスとして見えない個体は
  ソフトウェアでは救えない。対象は「見えるが読み出しが不安定」な個体まで。
- **TRIM 済み領域**: 外付け SSD などで TRIM が効いた領域は復元できない。
  USB メモリと SD カードでは通常 TRIM が効かないので問題になりにくい。
- **断片化した削除ファイル**: FAT32 も exFAT も、削除時にクラスタのつながりが消える。
  連続配置と仮定して回収するので、断片化していたファイルは壊れた状態で復元される。
  該当する項目には注記が付く。
- **NTFS / APFS / ext4**: 対応しない。対象は FAT32 と exFAT (USB メモリと SD カードの標準)。
  ファイルシステムを問わないシグネチャカービングは Phase 3 で入れる。

## 開発

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

CI (GitHub Actions) は windows-latest と macos-latest でテストと lint を回す。
壊れたメディアは CI に置けないので、不良セクタ・リトライ・再開は
`MockDevice` のエラー注入で検証している (`crates/ofr-image/tests/imaging.rs`)。

ファイルシステム解析のテストイメージは `ofr-testfs` が Rust だけで組み立てるので、
OS のフォーマッタを呼ばずに CI 内で完結する。生成物は本物の FAT32 / exFAT として
妥当なので、目視で確かめたいときはマウントできる。

```bash
cargo run -p ofr-testfs -- testdata/out
hdiutil attach -imagekey diskimage-class=CRawDiskImage -readonly testdata/out/fat32_deleted.img
```

### 実機での手動確認 (CI で回せない項目)

1. `ofr list` に USB メモリ / SD カードが出て、容量と種別が OS の表示 (`diskutil list` /
   `Get-Disk`) と一致すること。ここは管理者権限なしで動くこと。
2. 起動ディスクが「選択不可」と表示され、`ofr image` が終了コード 2 で拒否すること。
3. イメージングが完走し (終了コード 0)、mapfile が全域 `+` の 1 ブロックになり、
   イメージ長がデバイスサイズと一致すること。生デバイスとイメージを数か所で突き合わせること。
4. 途中で Ctrl-C して再実行すると、`mapfile から再開する rescued=...` が中断時点の
   取得バイト数と一致し、取得済み領域を読み直さずに再開すること。

```bash
# macOS (root 権限が要る)
sudo ofr image /dev/disk4 /Volumes/Backup/usb.img
```

```powershell
# Windows (管理者権限の PowerShell で)
.\ofr.exe image '\\.\PhysicalDrive2' C:\usbtest\usb.img
```

出力先は必ず復旧元と別のディスクにすること (同一デバイスならエラーになる)。

Phase 2 で足したぶんの手動確認:

5. 実際の USB メモリ / SD カードで `ofr scan` が本物のファイル一覧を出すこと。
   容量・クラスタサイズ・ラベルが OS の表示と一致すること。
6. デバイス上でファイルを削除してから `ofr scan --status deleted` で出ること。
   `ofr restore` で戻したファイルが元と同じ内容で開けること。
7. デバイスをクイックフォーマットしてから `ofr scan` で `Lost+Found/` 以下に
   フォルダが出ること。復元したファイルが開けること。
8. 復元先に復旧元と同じデバイス上のフォルダを指定すると、終了コード 2 で拒否すること。

7 は**実機でしか確認できない**。macOS のディスクイメージ (`hdiutil attach` した .img) を
`newfs_msdos` でフォーマットすると、イメージ側が TRIM を受けて全域ゼロになるため、
残骸が一切残らない。USB メモリと SD カードは通常 TRIM が効かないので実機では残る
(PLAN.md 10章)。イメージで試したいときは `cargo run -p ofr-testfs` が作る
`*_quick_format.img` を使うこと。こちらは実機のクイックフォーマットと同じく
FAT 表とルートだけを消してある。

Phase 1 時点での確認済み: macOS 26 / KIOXIA TransMemory 57.7 GiB (163 MiB/s で完走)、
Windows / USB メモリ。

### クレート構成

`ofr-device` (デバイス) → `ofr-image` (イメージング) / `ofr-fs` (解析の共通土台) →
`ofr-fat` `ofr-exfat` (ファイルシステム) → `ofr-cli`。

PLAN.md の構成に対して `ofr-fs` と `ofr-testfs` を足してある。前者は FAT32 と exFAT で
中身が同じになる部分 (中間表現、32bit FAT 表、パーティション解析、復元処理) の置き場で、
どちらか一方のクレートに寄せると他方が依存する形になるため独立させた。
後者はテストイメージ生成専用 (`publish = false`)。
