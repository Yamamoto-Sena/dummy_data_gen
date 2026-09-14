# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 概要

`dummy_data_gen` は、YAML設定ファイル（`schema.yaml`）で定義した列に沿ってダミーデータをCSV/SQL/JSON/Excel(xlsx)のいずれかに書き出す Rust 製の小さな CLI ツール。ソースは [src/main.rs](src/main.rs) の 1 ファイルのみで構成されている。

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

`--config` は列定義を書いたYAMLファイルのパス（デフォルト `schema.yaml`）。行数(`row_count`)や列(`columns`)はこのYAML側で定義する（CLI引数の `--rows` は廃止済み）。`--encoding` は `utf8` か `sjis`（Shift-JIS。Dr.Sum/MotionBoard等の日本の業務システム向け。`--format xlsx`では無視される）。`--format` は `csv`（デフォルト）/ `sql`（`INSERT INTO`文。`schema.yaml`に `table_name` の指定が必須）/ `json`（NDJSON、1行1件）/ `xlsx`（Excelファイル）。`--seed` は乱数シード（省略時は毎回ランダム。指定すると同じ設定で毎回同じデータが再現される）。`--output` で保存先パスを指定できる（省略時は形式に応じて `output.csv` / `output.sql` / `output.json` / `output.xlsx`）。行数が`PROGRESS_BAR_THRESHOLD`（1000）以上のとき、生成中にターミナルへ進捗バーを表示する（リダイレクト/非ttyでは自動的に非表示になる。indicatifの標準挙動）。

テストは `src/main.rs` 末尾の `#[cfg(test)] mod tests` にまとまっている（`cargo test` で実行）。バリデーション（min>max、不正な日付、null_rate/uniqueの範囲外など）、SQLエスケープ、CSV/JSON/ExcelのNULL表現、都道府県⇔市区町村の整合性、xlsxの読み返し検証（`calamine`使用）などをカバーしている。

## アーキテクチャ

- `Args`（clap の `derive` マクロを使用）が CLI引数 `--config` / `--encoding` / `--format` / `--seed` / `--output` を定義している。
- `Schema` / `ColumnDef` / `ColumnType`（serde の `Deserialize`）が `schema.yaml` の構造をそのまま表す。`load_schema` がファイル読み込み + YAMLパースを行う。
  `ColumnType` は `#[serde(tag = "type")]` の内部タグ付きenumで、`ColumnDef` 側は `#[serde(flatten)]` で受けている。
  そのため YAML上は `name` と `type` と、列タイプ固有の追加フィールド（`min`/`max`/`start`/`end`/`decimals`/`choices`）を同じ階層にフラットに書ける。
- 対応する列タイプ: `sequence`（連番）/ `name_ja`（日本語氏名）/ `email` / `integer`（min/max指定の整数）/ `float`（min/max/decimals指定の小数）/ `boolean` / `date`（start/end指定、`YYYY-MM-DD`）/ `postal_code`（日本の郵便番号風）/ `phone_ja`（携帯電話番号風）/ `address_ja`（都道府県+市区町村風の住所。1列で完結）/ `company_name_ja`（`LAST_NAMES`を再利用し「株式会社+姓+`COMPANY_SUFFIXES`」の形で生成）/ `uuid`（v4形式。`uuid::Uuid::new_v4()`は使わず、自前のシード付きrngで作った16バイトを`uuid::Builder::from_random_bytes`に渡すことで`--seed`の再現性を維持している）/ `prefecture_ja`（都道府県）/ `city_ja`（市区町村。**同じ行の直前にある`prefecture_ja`列の値に対応する市区町村を選ぶ**。無ければ全都道府県からランダム）/ `katakana_name`（フリガナ。**同じ行の直前にある`name_ja`列と同じ氏名の読みを選ぶ**。無ければランダム）/ `enum`（`choices`リストからランダムに1つ選ぶ、均等ランダムのみ対応。重み付けは非対応）。
- `prepare_columns` が `Schema`（YAMLそのまま、文字列ベース）を `PreparedColumn`（生成に使う実行時表現）に変換する。ここで min>max や日付の不正フォーマット・start>endなどのバリデーションを行い、日付は文字列を1行ごとに毎回パースし直さないよう、事前に「開始日からの通算日数(`start_days`)」と「日数の幅(`span_days`)」に変換しておく（大量行生成時の速度を落とさないため）。空の`columns`・列名の重複もここで弾く。
- **unique制約**: `ColumnDef.unique: bool`。`unique_capacity` が列タイプごとの「取りうる値の組み合わせ数」を計算できる型（`enum`/`boolean`/`integer`/`date`のみ。`postal_code`/`phone_ja`/`address_ja`/`float`は組み合わせが多すぎる・不連続で数えにくいため非対応）に限って対応する。`prepare_columns`で「row_countより組み合わせが少ない」「組み合わせが`UNIQUE_CAPACITY_CAP`(200万)を超える」「null_rateと併用している」の3パターンをエラーにする。実際の値は`resolve_unique_pools`（`base_seed`確定後に呼ぶ必要があるため`main`で`prepare_columns`の後に実行）が、`enumerate_values`で全候補を列挙→シャッフル→先頭row_count件を取る、という方式で一括生成し`PreparedColumn.unique_pool`に格納する。`generate_cell`は`unique_pool`があればそこから`row_num`に対応する値を返すだけになる（並列生成と両立させるため、unique列だけ事前に逐次で確定させておく設計）。
- `random_name` / `random_email` / `random_postal_code` / `random_phone` / `random_address` / `random_prefecture` / `random_city` / `random_katakana_name` が個別の値生成ロジック。`generate_value` が `PreparedColumnType` に応じてどれを呼ぶかを振り分ける。氏名・住所・電話番号などは `LAST_NAMES` / `FIRST_NAMES` / `CITIES_BY_PREFECTURE` / `PHONE_PREFIXES` の固定配列からのランダム組み合わせで、実在のデータとは無関係（あくまでダミー）。
- **列間整合性（F4-2, F1-4）**: 列を跨いだ整合性が必要な列タイプ(`city_ja`が`prefecture_ja`を、`katakana_name`が`name_ja`を参照する)は、`RowContext`構造体を介して実現している。
  ```rust
  struct RowContext { last_prefecture: Option<String>, last_name_indices: Option<(usize, usize)> }
  ```
  `generate_row`は列を先頭から順に処理し、`PrefectureJa`列の生成結果を`ctx.last_prefecture`に、`NameJa`列で実際に選ばれた姓・名の添字を`ctx.last_name_indices`に保存しながら`generate_cell(..., &ctx)`を呼ぶ。`generate_cell`は`CityJa`なら`ctx.last_prefecture`を、`KatakanaName`なら`ctx.last_name_indices`を渡して値を作る。氏名の読みは文字列からは逆引きできないため、`random_name_indices`が先に姓・名それぞれの添字を引き、`format_name`で文字列に組み立てる形に分離してある（`random_name`はこの薄いラッパー。乱数の消費順序＝姓→名は変えていないため、`--seed`指定時の出力に影響しない）。
  このため**参照される側の列（`prefecture_ja`/`name_ja`）は参照する側の列（`city_ja`/`katakana_name`）より前に定義する必要がある**（後ろにあると単に無視され、全体からランダムに選ぶフォールバック動作になる）。`ALL_CITIES`は`city_ja`単独使用時に使う、全都道府県の市区町村を1回だけ計算する`LazyLock`。
  順序を間違えた（＝参照先の列型は定義されているのに、参照する列より後ろにある）ケースは`misplaced_city_ja_warnings`/`misplaced_katakana_name_warnings`がそれぞれ検出し、`prepare_columns`の最後（`.inspect`）で警告文を`eprintln!`する。エラーにはしない（単独使用は正当な用途のため）。この2つの警告関数は同型だが、対象の型・メッセージが違うため無理に共通化していない。
  同じschemaに複数の`prefecture_ja`/`city_ja`ペア（または`name_ja`/`katakana_name`ペア）があっても、それぞれの参照列は直前の該当列に対応する（`ctx`のフィールドは毎回上書きされる）。`NameJa`が`unique_pool`経由またはNULLになった場合は添字を復元できないため`ctx.last_name_indices`は`None`になり、後続の`katakana_name`はフォールバック動作になる。
- `generate_cell` が1列分の値を作る（`null_rate` の確率で `None` を返す＝NULL）。戻り値は`(Option<String>, Option<(usize, usize)>)`で、2つ目は「`NameJa`として新たに選んだ姓・名の添字」（それ以外はNone）。`generate_row` がそれを使って`RowContext`を更新しつつ、全列分まとめて `Vec<Option<String>>` にする。`build_csv` はNoneを空文字に、`build_sql` はNoneをクォートなしの `NULL` に、`cell_to_json`/`write_xlsx_cell` はNoneをそれぞれJSONの`null`/Excelの空セルに変換する。
- `row_rng(base_seed, row_num)` が行番号ごとに独立した `SmallRng`（暗号強度は無いが高速なPRNG。ダミーデータ生成に暗号学的安全性は不要なため採用）を作る。`base_seed` が同じなら同じ行番号は常に同じ乱数列になるため、rayonでどのスレッドがどの行を処理しても結果が変わらない（再現性と並列化の両立）。`base_seed` は `--seed` 指定時はその値、未指定時は `rand::random()` で毎回変える。
- **`generate_all_rows(row_count, columns, base_seed)`** が全出力形式（CSV/SQL/JSON/Excel）で共有する行生成の一本化された入り口。`(1..=row_count).into_par_iter()`（rayon）で行ごとの値生成をCPUコア数に分散して並列に行い、`.collect()` で行番号順を保ったまま `Vec<Vec<Option<String>>>` にまとめる。ここで`new_progress_bar`が作った進捗バー（**F3-2**）の`.inc(1)`も呼ぶ。各`build_*`/`write_xlsx`関数はこの結果を受け取り、形式ごとの文字列/バイナリへの変換（直列処理）だけを行う。
- **進捗表示（F3-2）**: `new_progress_bar(row_count)`は`row_count < PROGRESS_BAR_THRESHOLD`（1000）なら`indicatif::ProgressBar::hidden()`を返す（一瞬で終わる少量データや`cargo test`実行時にバーが出て邪魔・出力が汚れるのを防ぐため）。それ以外は実際のバーを返す。indicatifは出力先が実ターミナルでない（リダイレクト・パイプ等）場合は自動的に描画をスキップするため、ログファイル等に制御文字が紛れ込むことはない。
- `build_csv` が `columns`（`&[PreparedColumn]`）からヘッダー行を作り、`generate_all_rows`の結果を `csv::Writer` でメモリ上のバッファに書き込み、UTF-8文字列として取り出す。
- `build_sql` は同じ`columns`/`row_count`から `INSERT INTO {table_name} (...) VALUES (...), (...), ...;` を組み立てる。1本のINSERT文に含める行数は `SQL_BATCH_SIZE`（1000）で区切っている。`is_text_column` が列タイプごとに `'...'` で囲むかどうかを判定し、`sql_literal` が値のクォート・エスケープ（`'` → `''`）を行う。
- **並列化の実測メモ**: 10列×100万行で計測したところ、値生成そのものは逐次実行で約1.55秒、rayon並列（8コア環境）で約0.55秒（約2.8倍）。ただし全体の処理時間は `std::fs::write` によるディスクへの実書き込みが約1〜1.4秒かかり支配的なため、体感の総時間短縮効果は限定的（並列化前後で3.155秒→2.6〜2.9秒程度）。CPUバウンドな部分は明確に速くなったが、ボトルネックは既にディスクI/O側に移っている、という計測結果に基づく判断。この傾向は`generate_all_rows`への一本化後も変わらない（再計測済み）。
- `build_json` は `serde_json::Map`（`preserve_order` featureで列の順序を保持）を使い、1行1件のNDJSON（`{"col":val, ...}\n{"col":val, ...}\n...`）を組み立てる。`cell_to_json` が列タイプに応じてJSONの型（数値/真偽値/文字列/null）を決める。
- **Excel出力（F2-2）**: `write_xlsx`が`rust_xlsxwriter::Workbook`を直接組み立てて保存する。CSV/SQL/JSONは「文字列を組み立ててから`write_text`で書き出す」という流れだが、xlsxはテキストではなくバイナリ(ZIP)形式なのでこの流れには乗らず、`write_xlsx`単体で完結している。そのため**`--encoding`はxlsxには効かない**（`main`でSJIS指定時に警告を出す）。`write_xlsx_cell`が列タイプに応じて`write_number`/`write_boolean`/`write_string`を使い分け、NULL(None)は何も書かず空セルのままにする。
- `write_text` が UTF-8文字列を受け取り、`encoding` に応じてそのまま保存するか `encoding_rs::SHIFT_JIS.encode()` でShift-JISに変換してから保存する共通処理（CSV/SQL/JSONで使う。xlsxは対象外）。`write_csv` / `write_sql` / `write_json` はそれぞれ `build_csv` / `build_sql` / `build_json` の結果をこれに渡すだけの薄いラッパー。
- `Args.format`（`csv` / `sql` / `json` / `xlsx`）で出力形式を切り替える。`sql` を選んだ場合、`schema.yaml` に `table_name` の指定がないとエラーで終了する。出力先は `--output` で明示的に指定でき、省略時は形式に応じて `output.csv` / `output.sql` / `output.json` / `output.xlsx`。
- エラーハンドリングは `Box<dyn std::error::Error>` + `?` によるシンプルな伝播で、`main` 側で `load_schema` → `prepare_columns` → `resolve_unique_pools` → (`write_csv`/`write_sql`/`write_json`/`write_xlsx`のいずれか) の各段階を順に処理し、失敗した時点でメッセージを表示して終了する。

## 依存クレート

- `clap`（derive feature）: CLI引数パース
- `rand` 0.8（`small_rng` feature）: ランダム値生成（`gen_range` API を使用しているため、`rand` を上げる場合は API 変更に注意）。`SmallRng` を行ごとのシード付き生成に使用。
- `csv`: CSV 書き込み
- `serde`（derive feature）/ `serde_yaml`: `schema.yaml` のパース（`serde_yaml` は deprecated 表記だが現状これを使用）
- `encoding_rs`: Shift-JIS (CP932) への文字コード変換
- `chrono`: `date` 列タイプの日付パース・計算（`Datelike` トレイトの `num_days_from_ce` を使用）
- `rayon`: 行ごとの値生成の並列化（`into_par_iter`）
- `serde_json`（`preserve_order` feature）: `--format json` のNDJSON出力
- `uuid`: `uuid`列タイプのUUID v4組み立て（`new_v4()`は使わず`Builder::from_random_bytes`のみ使用）
- `indicatif`: 進捗バー（`ProgressBar`）。行数がしきい値未満のときは`ProgressBar::hidden()`を使う
- `rust_xlsxwriter`: `--format xlsx`のExcelファイル生成
- `calamine`（`[dev-dependencies]`）: xlsx出力のテスト用（実際に書き出したファイルを読み返して検証する）

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
