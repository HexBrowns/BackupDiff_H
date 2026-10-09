# BackupDiff_H

AviUtl2 の自動バックアップ（`Backup/AutoBackup_*.aup2`）を 2 つ選び、オブジェクトを対応付けてから
効果・設定値の違いを一覧にする汎用プラグイン（Rust 製 `.aux2`）。元ネタは WinMerge。

- **バージョン:** 0.3.0（aviutl2-rs / aviutl2-eframe **0.48**。本体 **2.1.11** 以上）
- **仕様:** [`AI/specifications/20261005_BackupDiff_H_spec.md`](../../specifications/20261005_BackupDiff_H_spec.md)（第 3 段階まで）

## ビルド

```powershell
python AI/tools/au2_build.py BackupDiff_H               # テスト → au2 release → 本番（C:\ProgramData\aviutl2）へ配置
python AI/tools/au2_build.py BackupDiff_H --no-deploy   # 配置しない（本番との違いだけ出す）
cargo test                                              # このフォルダで
cargo test --release -- --ignored --nocapture real_backups   # 手元の Backup/ で一覧・パース・比較の時間を測る
```

`print_diff`（`--ignored`）は環境変数 `BD_A` / `BD_B` の 2 本を比べて結果を文字で出す。対応付けを目で確かめる用。

## 構成

| ファイル | 役割 |
|---|---|
| `src/lib.rs` | 登録、ウィンドウ、プロジェクトのパス（`on_project_load` / `on_project_save`） |
| `src/aup2.rs` | `.aup2` のパーサー（全キーを順序つきで保持）と、一覧用の軽い要約 |
| `src/backups.rs` | `Backup/` の列挙、`file=` によるプロジェクトの判定、共有読み取り |
| `src/diff.rs` | オブジェクトの対応付け（完全一致 → 移動 → 変更）と、効果・項目の比較 |
| `src/live.rs` | 本体とのやり取り。表示中のシーンの読み取り（`call_read_section`）と「戻す」（`call_edit_section`） |
| `src/history.rs` | 1 項目の移り変わり（バックアップを 1 本ずつたどる） |
| `src/inventory.rs` | 効果の棚卸し（効果ごとのプロジェクト数・オブジェクト数・最後に使った日時、CSV） |
| `src/dialog.rs` | Windows のダイアログ（フォルダ・CSV の保存先）と、ファイルの日時 |
| `src/worker.rs` | 履歴と棚卸しのスレッドの起こし方と、`Drop` での止め方 |
| `src/gui.rs` | egui のウィンドウ |

## 約束事

- **ファイルは読むだけ。** `Backup/` にも保存済みのプロジェクトにも書かない。ファイルは共有読み取りで開く
- 本体への書き込みは「戻す」を押したときの `call_edit_section` 1 回だけ（本体の Undo 1 回分）。書く前に今の値を読み直し、比べたときと違えば書かない
- 本体からの読み取りは、B に「編集中のシーン」を選んで「比較」を押したときの `call_read_section` だけ。イベント・タイマーからは呼ばない
- 比較はボタンを押したときに UI のスレッドで行う（実物で、一覧 100 本の初回が約 50ms、2 本のパースと比較が 10ms 未満。2026-10-05）
- 項目の履歴と棚卸しだけ裏のスレッドで作る（どちらも実物で 0.2 秒以下）。スレッドは `worker.rs` からだけ起こし、プラグインの `Drop` で止めて待つ
- 本体の効果の一覧（`get_effects()`）は「数える」を押したときだけ取る。ファイルのダイアログを初めて開くときに DLL を pin する
- 既定で比べない行: `focus=`、グループの開閉（`*.hide` / `Group` / `Group2` / `Group3`）、`[project]` の `file=` / `display.*` / `preview.*`、
  シーンの `cursor.*` / `preview.*` / `display.frame` / `display.layer` / `display.zoom` / `select.*`、`[plugin.N]`。「無視する行も出す」で全部比べる
- レイヤーは本体の表示に合わせて 1 始まり、フレームは `.aup2` の値のまま
