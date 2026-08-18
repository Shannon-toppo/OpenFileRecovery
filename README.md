# Open File Recovery

USB メモリと SD カードを主対象にしたデータ復旧ツール。Rust 製のコアと CLI、
のちに Tauri の GUI を載せる。設計と全体計画は [PLAN.md](PLAN.md) を参照。

MIT ライセンス。GPL/LGPL のコードは流用していない。外部バイナリ (ffmpeg など) にも依存しない。

## いまできること (Phase 1)

| コマンド | 内容 |
|---|---|
| `ofr list` | 接続されているデバイスの一覧。起動ディスクは選択不可として表示する |
| `ofr image` | 壊れかけメディアの吸い出し (ddrescue 方式の多段パス + mapfile による中断・再開) |

ファイルシステム解析 (Phase 2)、カービング (Phase 3)、コピー (Phase 4)、
修復 (Phase 5)、GUI (Phase 6) は未実装。

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

## 開発

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

CI (GitHub Actions) は windows-latest と macos-latest でテストと lint を回す。
壊れたメディアは CI に置けないので、不良セクタ・リトライ・再開は
`MockDevice` のエラー注入で検証している (`crates/ofr-image/tests/imaging.rs`)。

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

Phase 1 時点での確認済み: macOS 26 / KIOXIA TransMemory 57.7 GiB (163 MiB/s で完走)、
Windows / USB メモリ。
