# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 概要

`dummy_data_gen` は、YAML設定ファイル（`schema.yaml`）で定義した列に沿ってダミーデータをCSVに書き出す Rust 製の小さな CLI ツール。ソースは [src/main.rs](src/main.rs) の 1 ファイルのみで構成されている。

## 開発コマンド

```bash
cargo build
```

```bash
cargo run -- --config schema.yaml --encoding utf8 --format csv --seed 42
```

```bash
cargo test
```

`--config` は列定義を書いたYAMLファイルのパス（デフォルト `schema.yaml`）。行数(`row_count`)や列(`columns`)はこのYAML側で定義する（CLI引数の `--rows` は廃止済み）。`--encoding` は `utf8` か `sjis`（Shift-JIS。Dr.Sum/MotionBoard等の日本の業務システム向け）。`--format` は `csv`（デフォルト、`output.csv`）か `sql`（`output.sql`、`INSERT INTO`文。`schema.yaml`に `table_name` の指定が必須）。`--seed` は乱数シード（省略時は毎回ランダム。指定すると同じ設定で毎回同じデータが再現される）。出力パスは固定（`output.csv`/`output.sql`）で引数化されていない。

テストは `src/main.rs` 末尾の `#[cfg(test)] mod tests` にまとまっている（`cargo test` で実行）。バリデーション（min>max、不正な日付、null_rateの範囲外など）とSQLエスケープ、CSVの行数・NULL表現を中心にカバーしている。

## アーキテクチャ

- `Args`（clap の `derive` マクロを使用）が CLI引数 `--config` / `--encoding` / `--format` / `--seed` を定義している。
- `Schema` / `ColumnDef` / `ColumnType`（serde の `Deserialize`）が `schema.yaml` の構造をそのまま表す。`load_schema` がファイル読み込み + YAMLパースを行う。
  `ColumnType` は `#[serde(tag = "type")]` の内部タグ付きenumで、`ColumnDef` 側は `#[serde(flatten)]` で受けている。
  そのため YAML上は `name` と `type` と、列タイプ固有の追加フィールド（`min`/`max`/`start`/`end`/`decimals`）を同じ階層にフラットに書ける。
- 対応する列タイプ: `sequence`（連番）/ `name_ja`（日本語氏名）/ `email` / `integer`（min/max指定の整数）/ `float`（min/max/decimals指定の小数）/ `boolean` / `date`（start/end指定、`YYYY-MM-DD`）/ `postal_code`（日本の郵便番号風）/ `phone_ja`（携帯電話番号風）/ `address_ja`（簡易住所）。
- `prepare_columns` が `Schema`（YAMLそのまま、文字列ベース）を `PreparedColumn`（生成に使う実行時表現）に変換する。ここで min>max や日付の不正フォーマット・start>endなどのバリデーションを行い、日付は文字列を1行ごとに毎回パースし直さないよう、事前に「開始日からの通算日数(`start_days`)」と「日数の幅(`span_days`)」に変換しておく（大量行生成時の速度を落とさないため）。
- `random_name` / `random_email` / `random_postal_code` / `random_phone` / `random_address` が個別の値生成ロジック。`generate_value` が `PreparedColumnType` に応じてどれを呼ぶかを振り分ける。氏名・住所・電話番号などは `LAST_NAMES` / `FIRST_NAMES` / `PREFECTURES` / `CITIES` / `PHONE_PREFIXES` の固定配列からのランダム組み合わせで、実在のデータとは無関係（あくまでダミー）。
- `generate_cell` が1列分の値を作る（`null_rate` の確率で `None` を返す＝NULL）。`generate_row` がそれを全列分まとめて `Vec<Option<String>>` にする。`build_csv` はNoneを空文字に、`build_sql` はNoneをクォートなしの `NULL` に変換する。
- `row_rng(base_seed, row_num)` が行番号ごとに独立した `SmallRng`（暗号強度は無いが高速なPRNG。ダミーデータ生成に暗号学的安全性は不要なため採用）を作る。`base_seed` が同じなら同じ行番号は常に同じ乱数列になるため、rayonでどのスレッドがどの行を処理しても結果が変わらない（再現性と並列化の両立）。`base_seed` は `--seed` 指定時はその値、未指定時は `rand::random()` で毎回変える。
- `build_csv` / `build_sql` はどちらも `(1..=row_count).into_par_iter()`（rayon）で行ごとの値生成をCPUコア数に分散して並列に行い、`.collect()` で行番号順を保ったまま `Vec` にまとめる。ファイルへの直列な書き込み（`csv::Writer` へのループ、SQLの文字列連結）はこの後に1回だけ行う。
- `build_csv` が `columns`（`&[PreparedColumn]`）からヘッダー行を作り、並列生成した行を `csv::Writer` でメモリ上のバッファに書き込み、UTF-8文字列として取り出す。
- `build_sql` は同じ`columns`/`row_count`から `INSERT INTO {table_name} (...) VALUES (...), (...), ...;` を組み立てる。1本のINSERT文に含める行数は `SQL_BATCH_SIZE`（1000）で区切っている。`is_text_column` が列タイプごとに `'...'` で囲むかどうかを判定し、`sql_literal` が値のクォート・エスケープ（`'` → `''`）を行う。
- **並列化の実測メモ**: 10列×100万行で計測したところ、値生成そのものは逐次実行で約1.55秒、rayon並列（8コア環境）で約0.55秒（約2.8倍）。ただし全体の処理時間は `std::fs::write` によるディスクへの実書き込みが約1〜1.4秒かかり支配的なため、体感の総時間短縮効果は限定的（並列化前後で3.155秒→2.6〜2.9秒程度）。CPUバウンドな部分は明確に速くなったが、ボトルネックは既にディスクI/O側に移っている、という計測結果に基づく判断。
- `write_text` が UTF-8文字列を受け取り、`encoding` に応じてそのまま保存するか `encoding_rs::SHIFT_JIS.encode()` でShift-JISに変換してから保存する共通処理（CSV/SQLどちらの出力でも使う）。`write_csv` / `write_sql` はそれぞれ `build_csv` / `build_sql` の結果をこれに渡すだけの薄いラッパー。
- `Args.format`（`csv` / `sql`）で出力形式を切り替える。`sql` を選んだ場合、`schema.yaml` に `table_name` の指定がないとエラーで終了する。出力先は形式に応じて `output.csv` / `output.sql` に固定。
- エラーハンドリングは `Box<dyn std::error::Error>` + `?` によるシンプルな伝播で、`main` 側で `load_schema` → `prepare_columns` → (`write_csv` または `write_sql`) の各段階を順に `match` し、失敗した時点でメッセージを表示して終了する。

## 依存クレート

- `clap`（derive feature）: CLI引数パース
- `rand` 0.8（`small_rng` feature）: ランダム値生成（`gen_range` API を使用しているため、`rand` を上げる場合は API 変更に注意）。`SmallRng` を行ごとのシード付き生成に使用。
- `csv`: CSV 書き込み
- `serde`（derive feature）/ `serde_yaml`: `schema.yaml` のパース（`serde_yaml` は deprecated 表記だが現状これを使用）
- `encoding_rs`: Shift-JIS (CP932) への文字コード変換
- `chrono`: `date` 列タイプの日付パース・計算（`Datelike` トレイトの `num_days_from_ce` を使用）
- `rayon`: 行ごとの値生成の並列化（`into_par_iter`）

## 優先順位

既存コードと一般的なフレームワークの慣習が衝突したとき、以下の順で優先する。

1. 既存コード（リポジトリ上の実装・慣習）
2. CLAUDE.md のルール
3. 一般的なフレームワークの慣習

## 差分最小の原則

- ファイルの移動・名前変更・大規模リファクタは禁止
- 新規レイヤ（Service層等）の導入禁止（必要なら提案のみ）
- 1タスクで触る範囲は目的に直結する箇所に限定

## 完了条件

- 動作検証（コマンド実行・テスト等）で証明するまで完了としない

## 出力ルール

- ファイル丸ごとの出力禁止。変更箇所周辺のみ表示する
- 変更のない行は省略記法を使う（`// ... existing code ...` 等）

## 調査プロセス

- いきなり全ファイルを読まず、検索で候補を絞ってから読む
- 大規模な変更を一括提案せず「まずAを確認 → 結果を見てBを修正」と1手ずつ進める

## 不具合修正の停止条件

以下の場合は必ず作業を止めて報告し、指示を待つ：

- 修正により別の不具合が新規発生した場合が2回連続したとき
- 同一事象に対して3サイクル目（調査→修正→検証）に入るとき

## その他

- `edition = "2024"`（Cargo.toml）のため、対応する Rust ツールチェーンが必要。
- コード中のコメントは日本語で、Rust初学者向けに構文の意味を説明する目的で書かれている（例: `?` 演算子の説明）。同様のスタイルを踏襲する場合はこの調子で。
