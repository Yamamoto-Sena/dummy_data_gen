# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 概要

`dummy_data_gen` は、YAML設定ファイル（`schema.yaml`）で定義した列に沿ってダミーデータをCSV/SQL/JSON/Excel(xlsx)のいずれかに書き出す Rust 製の小さな CLI ツール。

生成ロジック本体は [src/lib.rs](src/lib.rs) に置かれており、外部のクレート（例えば `C:\dev_2\dummygen_jp_gui` の Tauri+React GUI）から `dummy_data_gen` をライブラリとして呼び出せる。[src/main.rs](src/main.rs) はCLI引数(`Args`)の定義と`main()`のみを持つ薄いラッパーで、実際の処理はすべて`dummy_data_gen::`（lib.rs側）の関数を呼び出しているだけ。CLIとしての挙動（コマンド・出力・エラーメッセージ）はこの分割の前後で一切変わっていない。

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
- `Schema` / `ColumnDef` / `ColumnType`（serde の `Deserialize`）が「1テーブル分の定義」を表す（`Schema.table_name`は`#[serde(alias = "name")]`で`tables:`形式の`name:`キーも受ける）。`load_schema` は `RawSchemaFile`（トップレベルの`row_count`/`table_name`/`columns`/`tables`を全部Optionとして受ける箱）にパースしてから `normalize_schema_file` に渡し、単一テーブル形式・`tables:`形式のどちらも `SchemaFile { tables: Vec<Schema>, multi_table: bool }` に正規化する（**F4-3**。詳細は下の「複数テーブルの外部キー整合性」を参照）。単一/複数の判定に`serde(untagged)`を使わないのは、untaggedだと「どのバリアントにも一致しません」という分かりにくいエラーになり、既存の日本語エラーメッセージの質が落ちるため。
  `ColumnType` は `#[serde(tag = "type")]` の内部タグ付きenumで、`ColumnDef` 側は `#[serde(flatten)]` で受けている。
  そのため YAML上は `name` と `type` と、列タイプ固有の追加フィールド（`min`/`max`/`start`/`end`/`decimals`/`choices`）を同じ階層にフラットに書ける。
- 対応する列タイプ: `sequence`（連番）/ `name_ja`（日本語氏名フルネーム。`with_space: bool`省略可・既定false。trueで姓と名の間に半角スペースを入れる、例:「山田 太郎」）/ `last_name_ja`（姓のみ）/ `first_name_ja`（名のみ、`last_name_ja`/`first_name_ja`はどちらも列間の対応付けはしない独立乱数）/ `romaji_name`（氏名のローマ字・ヘボン式、「姓 名」の順で常にスペース区切り。**同じ行の直前にある`name_ja`列と同じ氏名のローマ字を選ぶ**。無ければランダム。`katakana_name`と同じ参照ロジック）/ `katakana_last_name`/`katakana_first_name`（フリガナの姓・名単独。`last_name_ja`/`first_name_ja`と同様、列間の対応付けをしない独立乱数）/ `email`（`domain`省略可・既定`example.com`）/ `integer`（min/max指定の整数）/ `float`（min/max/decimals指定の小数）/ `boolean` / `gender`（"男性"/"女性"を50%ずつのランダムで返す。列間の連動は無い独立乱数）/ `blood_type`（"A型"/"O型"/"B型"/"AB型"を日本人の血液型分布の目安(4:3:2:1)で重み付けして返す。`enum`のweightsと同じ`choose_weighted`を使うが、選択肢・重みは固定で設定不要。`with_suffix: bool`省略可・既定true。falseで「型」を付けずに"A"/"O"/"B"/"AB"を返す）/ `date`（start/end指定、`format`省略可: `ymd`(既定, `YYYY-MM-DD`) / `iso8601`(日付のみでは`ymd`と同一表記) / `slash`(`YYYY/MM/DD`)）/ `birth_date`（`min_age`/`max_age`で年齢範囲を指定し、「今日」基準で日付範囲を逆算する。365日/年の近似計算。`format`はdateと共通）/ `postal_code`（日本の郵便番号風。`with_hyphen: bool`省略可・既定true。falseで「-」を入れず`NNNNNNN`にする）/ `phone_ja`（携帯電話番号風。`with_hyphen: bool`省略可・既定true。falseで「-」を入れない）/ `phone_ja_landline`（固定電話番号風、市外局番は`PHONE_PREFIXES_LANDLINE`。`with_hyphen`は`phone_ja`と同じ意味）/ `address_ja`（都道府県+市区町村風の住所。1列で完結。`allowed_prefectures: Option<Vec<String>>`省略可・既定None(47都道府県すべて対象)。指定した都道府県名だけからランダムに選ぶ）/ `company_name_ja`（`LAST_NAMES`を再利用し「株式会社+姓+`COMPANY_SUFFIXES`」の形で生成）/ `uuid`（v4形式。`uuid::Uuid::new_v4()`は使わず、自前のシード付きrngで作った16バイトを`uuid::Builder::from_random_bytes`に渡すことで`--seed`の再現性を維持している）/ `prefecture_ja`（都道府県。`allowed_prefectures`は`address_ja`と同じ意味・同じ既定値）/ `city_ja`（市区町村。**同じ行の直前にある`prefecture_ja`列の値に対応する市区町村を選ぶ**。無ければ全都道府県からランダム）/ `katakana_name`（フリガナ全角。**同じ行の直前にある`name_ja`列と同じ氏名の読みを選ぶ**。無ければランダム。`with_space: bool`省略可・既定false。trueで姓の読みと名の読みの間に半角スペースを入れる、例:「ヤマダ タロウ」。`name_ja`の`with_space`と同じ考え方で、値そのものが独立しているため`name_ja`側のON/OFFとは連動しない）/ `katakana_name_hankaku`（フリガナ半角。`katakana_name`と同じ参照ロジックの上で`KATAKANA_FULL_TO_HALF`対応表により全角→半角変換する。辞書拡充時はこの対応表も見直すこと。`hankaku_katakana_table_covers_all_dictionary_characters`テストが未対応文字を検知する。`with_space`も`katakana_name`と同じ意味を持つが、区切りの半角スペースは`KATAKANA_FULL_TO_HALF`に無い文字のため、姓の読み・名の読みをそれぞれ個別に半角変換してから連結する必要がある（`random_katakana_name_hankaku`。全角のまま連結してから変換すると区切りのスペースが変換表に無い文字として落ちてしまうため、`random_katakana_name`とは別関数にしてある）)/ `credit_card_number` / `credit_card_expiry`（`MM/YY`形式、「今日」から1〜5年後のランダムな年月。`with_slash: bool`省略可・既定true。falseで「/」を入れず`MMYY`にする）/ `bank_account_number`（日本の普通預金口座番号を想定した7桁ゼロ埋め数字）/ `product_sku`（`"SKU-"`+英大文字/数字8文字、`random_api_key`と同じ乱数の使い方）/ `my_number` / `enum`（`choices`リストからランダムに1つ選ぶ。`weights: Option<Vec<f64>>`省略可・`choices`と同じ個数で出現比率を指定できる（`rand::seq::SliceRandom::choose_weighted`使用）。省略時は均等ランダム。`prepare_columns`で個数一致・非負・合計>0を検証する）/ `fixed`（`value`で指定した文字列を毎行そのまま返す）/ `pattern`（`pattern`に指定した正規表現に似た簡易記法から値を生成する。例: `"[A-Z]{3}-[0-9]{4}"` → `"ABC-1234"`。対応する構文はリテラル文字・`\`エスケープ・`[...]`文字クラス(範囲`a-z`・`^`否定)・量指定子`?`/`*`/`+`/`{n}`/`{n,}`/`{n,m}`のみで、グループ化`(...)`や選択`|`は非対応。`compile_pattern`が事前にコンパイルし、`PreparedColumnType::Pattern`が`Vec<PatternPiece>`(文字候補+繰り返し回数)として保持する）/ `username`（`FIRST_NAMES_ROMAJI`を小文字化+3桁の数字。例: `"taro123"`）/ `password`（英大小文字・数字・一部記号12文字のダミー値）/ `profile_image_url`（プレースホルダー画像サービス`placehold.jp`のURL文字列。実際に画像を取得したりHTTP通信をしたりはしない）/ `foreign_key`（`references: "テーブル名.列名"`形式。複数テーブル形式`tables:`専用、詳細は下記「複数テーブルの外部キー整合性」参照）/ `correlated_number`（他の列の値から計算する数値。**同じ行の中で自分より前に定義された列だけを参照できる**(prefecture_ja→city_jaと同じ制約だが、こちらは違反すると警告ではなくエラーになる)。`base_columns`(掛け合わせる数値列名のリスト。integer/float/sequence/correlated_numberのみ指定可、1つ以上必須)、`category_column`+`category_multipliers`(省略可。指定列の実際の値ごとに倍率を変える。一致しない値は倍率1.0)、`date_column`+`monthly_multipliers`(省略可。指定したdate/birth_date列の月によって倍率を変える12個の数値)、`noise`(省略可・既定0.0。±noiseのランダムな乗数)、`decimals`(省略可・既定0)、`min`/`max`(省略可のクランプ)。詳細は下記「相関のある数値(correlated_number)」参照）。
  氏名辞書（`LAST_NAMES`/`FIRST_NAMES`/`LAST_NAMES_KANA`/`FIRST_NAMES_KANA`）は姓30件・名20件に拡充済み（元は5件ずつ）。`LAST_NAMES_ROMAJI`/`FIRST_NAMES_ROMAJI`は同じ並び順・添字対応の手書きローマ字表（カタカナ→ローマ字の自動変換は促音・拗音・長音の扱いが複雑になるため、既存のカナ辞書と同様に手書きにしてある）。
  `katakana_name`/`katakana_name_hankaku`/`romaji_name`はいずれも「自分より前のname_ja列を参照する」設計が共通のため、`misplaced_katakana_name_warnings`が3型まとめて同じロジックで警告を出す（内部の`type_label`ヘルパーで列タイプごとのメッセージ文言を出し分ける）。
- `prepare_columns` が `Schema`（YAMLそのまま、文字列ベース）を `PreparedColumn`（生成に使う実行時表現）に変換する。ここで min>max や日付の不正フォーマット・start>endなどのバリデーションを行い、日付は文字列を1行ごとに毎回パースし直さないよう、事前に「開始日からの通算日数(`start_days`)」と「日数の幅(`span_days`)」に変換しておく（大量行生成時の速度を落とさないため）。空の`columns`・列名の重複もここで弾く。
- **unique制約**: `ColumnDef.unique: bool`。`unique_capacity`が列タイプごとの対応方法を`UniqueCapacity`(`Enumerable(容量)`/`Retry(容量)`/`Unsupported`)として返す。
  - `Enumerable`: 組み合わせ数が少なく全列挙できる型（`enum`/`boolean`/`gender`(2通り)/`blood_type`(4通り)/`integer`/`date`/`birth_date`(dateと同じ日数分)/`name_ja`(姓30×名20=600通り)/`last_name_ja`/`first_name_ja`/`romaji_name`(600通り)/`katakana_last_name`/`katakana_first_name`/`prefecture_ja`(`CITIES_BY_PREFECTURE.len()`通り。`allowed_prefectures`で絞り込んでいればその件数)/`company_name_ja`(姓30×`COMPANY_SUFFIXES`8=240通り)/`department_ja`(`DEPARTMENTS.len()`通り)/`job_title_ja`(`JOB_TITLES.len()`通り)/`credit_card_expiry`(1〜5年後×1〜12月=60通り)/`fixed`(常に同じ値なので組み合わせは常に1通り)）。`enumerate_values`で全候補を列挙→シャッフル→先頭row_count件を取る。組み合わせが`UNIQUE_CAPACITY_CAP`(200万)を超えるとエラー。`enum`に`weights`が指定されていてもこの方式では無視される（`unique_capacity`/`enumerate_values`の`PreparedColumnType::Enum`パターンは`weights`を無視するだけで、エラーにはしない）。
  - `Retry`: 組み合わせ数が膨大でEnumerable方式では列挙しきれないが、`row_count`(最大100万)に対しては十分大きい型（`phone_ja`/`phone_ja_landline`(市外局番の数×10^8通り)/`postal_code`(1000万通り)/`address_ja`(都道府県ごとの市区町村数の合計×19×19通り。`allowed_prefectures`で絞り込んでいれば対象都道府県分だけの合計)/`uuid`/`jwt`/`api_key`(いずれも正確な組み合わせ数がu128の範囲を超えて計算できないほど巨大なため`u128::MAX`で代用)/`ip_address`(256^4通り)/`username`(20×999=19,980通り)/`password`(68^12通り)/`profile_image_url`(約300万通り)/`credit_card_number`(10^14通り)/`bank_account_number`(10^7通り)/`product_sku`(36^8通り)/`my_number`(10^11通り)）。`build_unique_pool_by_retry`が値を生成→`HashSet`で既出チェック→被っていたら作り直す、をrow_count件集まるまで繰り返す。`UNIQUE_CAPACITY_CAP`は対象外（列挙しないため）。試行回数の上限(`row_count×1000`か10万の大きい方)は無限ループ防止の安全弁で、事前の容量検証を正しく通過している限り実務上到達しない。
  - `float`は`unique_capacity`が`min`/`max`/`decimals`から組み合わせ数(`((max-min)*10^decimals).round()+1`)を計算し、`UNIQUE_CAPACITY_CAP`以下なら`Enumerable`、それを超えれば`Retry`を返す(integerと同じ考え方だが、範囲・桁数次第でどちらにもなりうる唯一の型)。`((*max - *min) * scale).round() as u128`は浮動小数点→整数の`as`キャスト(Rustでは飽和キャストでパニックしない)を利用しており、NaNや極端に大きい値になっても`0`または`u128::MAX`に丸まって安全側(`Retry`)に倒れる。`enumerate_values`のFloat分岐は、浮動小数点の蓄積誤差でmin〜maxの間の値が飛んだり文字列が重複したりしないよう、`min`/`max`を整数(`i128`)にスケールしてから`(min_scaled..=max_scaled)`で回し、最後にだけ小数へ戻して`format!`する設計にしてある。
  - `sequence`/`email`は`Unsupported`のままだが、これは非対応というより「仕組み上すでに値が絶対に重複しない（`sequence`は連番そのもの、`email`は`random_email(row_num, domain)`が行番号をそのまま使う）ためunique自体が不要」という扱い。GUI(`dummygen_jp_gui/src/ColumnRow.tsx`の`ALWAYS_UNIQUE_REASONS`)ではこの2型だけチェックボックスの代わりに理由を一言表示する。
  - `pattern`(組み合わせ数の計算が複雑)/`city_ja`・`katakana_name`・`katakana_name_hankaku`(直前の列を参照する型で、事前に確定させるunique_poolの設計と相性が悪いため)/`foreign_key`(1つの親の値を複数の子行が参照するのが外部キーの通常の挙動のため、別途明示的にエラーにしている)/`correlated_number`(計算結果に対して事前に全パターンを列挙するのが現実的でないため)は`Unsupported`のまま。
  - `prepare_columns`で「`Unsupported`」「`Enumerable`で組み合わせが`UNIQUE_CAPACITY_CAP`超」「組み合わせがrow_countより少ない(EnumerableとRetry共通)」「null_rateと併用している」の4パターンをエラーにする。実際の値は`resolve_unique_pools`（`base_seed`確定後に呼ぶ必要があるため`main`で`prepare_columns`の後に実行）が`build_unique_pool`（内部で上記2方式に振り分け）で一括生成し`PreparedColumn.unique_pool`に格納する。`generate_cell`は`unique_pool`があればそこから`row_num`に対応する値を返すだけになる（並列生成と両立させるため、unique列だけ事前に逐次で確定させておく設計）。
- **name_ja のスペース有無**: `ColumnType::NameJa { with_space: bool }`(`#[serde(default)]`、省略時false)。`with_space: true`のとき`format_name`が姓と名の間に半角スペースを入れる（例:「山田 太郎」）。省略時は既存のschema.yamlと同じ「山田太郎」（スペース無し）のまま。
- **列ごとのデータ型上書き指定**: `ColumnDef.data_type: Option<String>`(`#[serde(default)]`、列タイプに関係なく全列共通。省略時None)。SQL(`sql_literal`)/JSON(`cell_to_json`)/Excel(`write_xlsx_cell`)が値を文字列/整数/小数/真偽値のどれとして出力するかを、列タイプからの自動判定(`default_value_category`)から上書きする。`resolve_value_category(kind, data_type)`がこの3関数共通の判定の入口で、`data_type`が`Some`なら`classify_data_type_name`(自由入力の型名の先頭単語から`ValueCategory`を判定。`INT`系→整数、`FLOAT`/`DECIMAL`系→小数、`BOOL`系→真偽値、それ以外(`VARCHAR`/`DATE`等、未知の型名含む)→文字列)、`None`なら`default_value_category(kind)`(今までの自動判定そのもの)を返す。CSV出力には影響しない(CSVには元々型の区別が無いため)。案件によって「この列は本当はこの型として扱ってほしい」という決まりがある場合に使う(例: `postal_code`を`with_hyphen: false`にしたうえで`data_type: "INTEGER"`にすると、SQLでクォート無し・JSONで数値になる)。
- `random_name` / `random_email` / `random_postal_code` / `random_phone` / `random_address` / `random_prefecture` / `random_city` / `random_katakana_name` が個別の値生成ロジック。`generate_value` が `PreparedColumnType` に応じてどれを呼ぶかを振り分ける。氏名・住所・電話番号などは `LAST_NAMES` / `FIRST_NAMES` / `CITIES_BY_PREFECTURE` / `PHONE_PREFIXES` の固定配列からのランダム組み合わせで、実在のデータとは無関係（あくまでダミー）。
- **列間整合性（F4-2, F1-4）**: 列を跨いだ整合性が必要な列タイプ(`city_ja`が`prefecture_ja`を、`katakana_name`が`name_ja`を参照する)は、`RowContext`構造体を介して実現している。
  ```rust
  struct RowContext { last_prefecture: Option<String>, last_name_indices: Option<(usize, usize)> }
  ```
  `generate_row`は列を先頭から順に処理し、`PrefectureJa`列の生成結果を`ctx.last_prefecture`に、`NameJa`列で実際に選ばれた姓・名の添字を`ctx.last_name_indices`に保存しながら`generate_cell(..., &ctx)`を呼ぶ。`generate_cell`は`CityJa`なら`ctx.last_prefecture`を、`KatakanaName`なら`ctx.last_name_indices`を渡して値を作る。氏名の読みは文字列からは逆引きできないため、`random_name_indices`が先に姓・名それぞれの添字を引き、`format_name`で文字列に組み立てる形に分離してある（`random_name`はこの薄いラッパー。乱数の消費順序＝姓→名は変えていないため、`--seed`指定時の出力に影響しない）。
  このため**参照される側の列（`prefecture_ja`/`name_ja`）は参照する側の列（`city_ja`/`katakana_name`）より前に定義する必要がある**（後ろにあると単に無視され、全体からランダムに選ぶフォールバック動作になる）。`ALL_CITIES`は`city_ja`単独使用時に使う、全都道府県の市区町村を1回だけ計算する`LazyLock`。`CITIES_BY_PREFECTURE`は47都道府県すべてに対応済み（元は東京都・大阪府・愛知県・北海道・福岡県の5件のみだった）で、各都道府県5件ずつ実在する市区町村名を載せている。件数・並び順が`--seed`指定時の再現性に影響するため、拡充前に作られたseed付きデータとは都道府県・市区町村の出現結果が変わる（氏名辞書を拡充したときと同じ、既知の仕様）。
  順序を間違えた（＝参照先の列型は定義されているのに、参照する列より後ろにある）ケースは`misplaced_city_ja_warnings`/`misplaced_katakana_name_warnings`がそれぞれ検出し、`prepare_columns`の最後（`.inspect`）で警告文を`eprintln!`する。エラーにはしない（単独使用は正当な用途のため）。この2つの警告関数は同型だが、対象の型・メッセージが違うため無理に共通化していない。
  同じschemaに複数の`prefecture_ja`/`city_ja`ペア（または`name_ja`/`katakana_name`ペア）があっても、それぞれの参照列は直前の該当列に対応する（`ctx`のフィールドは毎回上書きされる）。`NameJa`が`unique_pool`経由またはNULLになった場合は添字を復元できないため`ctx.last_name_indices`は`None`になり、後続の`katakana_name`はフォールバック動作になる。
- **相関のある数値(correlated_number)**: `city_ja`/`katakana_name`と違い、参照する列名を利用者がYAMLで自由に指定できる(`RowContext`のような「直前の1件だけ覚える」設計では対応できない)。そのため`generate_cell`の引数に`preceding_columns: &[PreparedColumn]`/`preceding_values: &[Option<String>]`(「この列より前の列」の定義と、その行での生成済みの値。同じ添字で対応する)を追加し、`generate_row`が`for (i, column) in columns.iter().enumerate()`で`&columns[..i]`と`&values`(その時点でi件積み上がっている)を渡す。`generate_correlated_number`が列名から値を探す(`preceding_columns.iter().position(...)`→`preceding_values[i]`)、`base_columns`の値を掛け算→`category_column`/`category_multipliers`で倍率→`date_column`/`monthly_multipliers`で季節倍率(`extract_month_from_date_string`が`DateFormat`ごとにフォーマット済み文字列から月を取り出す。`format_date`の逆方向の簡易パーサ)→`noise`のランダムな乗数→`min`/`max`でクランプ、の順に計算する。参照する列(`base_columns`/`category_column`/`date_column`)は`prepare_columns`が「この列より前に実在するか」「型が適切か(base_columnsは数値、date_columnはdate/birth_date)」を検証し、違反すると**エラー**になる(city_ja/katakana_nameは順序違反でも警告止まりでフォールバックするが、correlated_numberは計算そのものが成立しないため警告では済ませられない)。`default_value_category`では常に`Float`(SQL/JSON/Excelで数値として出力される)。unique制約は非対応（上記の`Unsupported`一覧を参照）。
- `generate_cell` が1列分の値を作る（`null_rate` の確率で `None` を返す＝NULL）。戻り値は`(Option<String>, Option<(usize, usize)>)`で、2つ目は「`NameJa`として新たに選んだ姓・名の添字」（それ以外はNone）。`generate_row` がそれを使って`RowContext`を更新しつつ、全列分まとめて `Vec<Option<String>>` にする。`build_csv` はNoneを空文字に、`build_sql` はNoneをクォートなしの `NULL` に、`cell_to_json`/`write_xlsx_cell` はNoneをそれぞれJSONの`null`/Excelの空セルに変換する。
- `row_rng(base_seed, row_num)` が行番号ごとに独立した `SmallRng`（暗号強度は無いが高速なPRNG。ダミーデータ生成に暗号学的安全性は不要なため採用）を作る。`base_seed` が同じなら同じ行番号は常に同じ乱数列になるため、rayonでどのスレッドがどの行を処理しても結果が変わらない（再現性と並列化の両立）。`base_seed` は `--seed` 指定時はその値、未指定時は `rand::random()` で毎回変える。
- **`generate_all_rows(row_count, columns, base_seed)`** が全出力形式（CSV/SQL/JSON/Excel）で共有する行生成の一本化された入り口。`(1..=row_count).into_par_iter()`（rayon）で行ごとの値生成をCPUコア数に分散して並列に行い、`.collect()` で行番号順を保ったまま `Vec<Vec<Option<String>>>` にまとめる。ここで`new_progress_bar`が作った進捗バー（**F3-2**）の`.inc(1)`も呼ぶ。**`main`はこれを1回だけ呼び**、`--format`で複数形式を指定してもその結果(`rows`)を使い回す（**F3-3**。形式を増やしても値生成のコストは増えない）。
- **進捗表示（F3-2）**: `new_progress_bar(row_count)`は`row_count < PROGRESS_BAR_THRESHOLD`（1000）なら`indicatif::ProgressBar::hidden()`を返す（一瞬で終わる少量データや`cargo test`実行時にバーが出て邪魔・出力が汚れるのを防ぐため）。それ以外は実際のバーを返す。indicatifは出力先が実ターミナルでない（リダイレクト・パイプ等）場合は自動的に描画をスキップするため、ログファイル等に制御文字が紛れ込むことはない。
- **`*_from_rows`関数群（F3-3で導入）**: `build_csv_from_rows` / `build_sql_from_rows` / `build_json_from_rows` / `write_xlsx_from_rows` が、`generate_all_rows`の結果(`rows`)を受け取って清書するだけの関数。`main`はこれらを直接呼ぶ。`build_csv` / `build_sql` / `build_json` / `write_xlsx`（引数に`row_count`/`base_seed`を取り内部で`generate_all_rows`を呼ぶ「一発で作れる」版）は本体からは使われなくなったため`#[cfg(test)]`を付けてテスト専用にしてある（既存25件のテストを書き換えずに済ませるため）。`write_csv`/`write_sql`/`write_json`（旧: `build_*`→`write_text`の薄いラッパー）はどこからも呼ばれなくなったため削除済み。
- **ストリーミング書き込み（GUI向け）**: `write_csv_streaming`/`write_sql_streaming`は、`generate_all_rows`のように全行を一度にメモリへ載せず、`chunk_size`行（既定`DEFAULT_CHUNK_SIZE`=1000の倍数、GUI側は10,000を使用）ごとに「`generate_rows_range`(絶対行番号の範囲を受け取る内部ヘルパー、`generate_all_rows`とは独立)で生成→即座に`BufWriter`へ書き込み→破棄」を繰り返す。CLI(`main.rs`)がCSV/SQL単体出力のときに使う（JSON/XLSX混在・複数形式同時出力・複数テーブルは従来通り`generate_all_rows`を使う）ほか、`dummygen_jp_gui`のTauriコマンドからも呼ばれる。`chunk_size`が`SQL_BATCH_SIZE`(1000)の倍数である限り、SQLのINSERT文の区切り方は一括生成と完全に一致する（chunk_sizeがずれると壊れはしないがINSERT文の区切りが変わる）。進捗は`on_progress: impl FnMut(u64, u64)`（完了行数, 全行数）で通知する。
  `write_csv_streaming`は`write_bom: bool`引数を持ち、trueかつUTF-8のときだけファイル先頭にUTF-8のBOM(`EF BB BF`)を書く。日本語版ExcelはBOムの無いUTF-8のCSVをダブルクリックで開くとShift-JISとして誤読し文字化けするため、GUIから呼ぶときはtrueを渡している。CLI(`main.rs`)からはfalseを渡し、既存の出力バイト列を変えない。
  `write_csv_streaming`はさらに`quote_all: bool`引数を持ち、trueのとき`csv::WriterBuilder`の`quote_style`を`QuoteStyle::Always`にして全フィールドをダブルクォートで囲む(false=既定は`QuoteStyle::Necessary`で、カンマ・改行・`"`を含む値だけを囲む従来通りの挙動)。名称にスペースを含む値の区切りを明確にしたい用途向け。CLIには`--quote-all`フラグ(既定false)、GUIには単一テーブルの`GenerateRequest.quote_all`/複数テーブルの`GenerateRequestMulti.quote_all`として対応している。SQL出力(`write_sql_streaming`)には影響しない(文字列値はもともと`'`で囲まれるため)。`build_csv_from_rows`(複数形式同時出力・複数テーブル用の一括生成パス)も同じ`quote_all: bool`引数を持ち、`write_output_multi_table`のCsv分岐がそのまま渡す(単一テーブルの複数形式同時出力を行う`write_output`のCsv分岐は、CLIの「`--format csv`単体のときのみ有効」という既存の制約を保つため、常にfalseを渡す)。
- `build_csv_from_rows` が `columns`（`&[PreparedColumn]`）からヘッダー行を作り、`rows` を `csv::Writer` でメモリ上のバッファに書き込み、UTF-8文字列として取り出す。
- `build_sql_from_rows` は同じ`columns`/`rows`から `INSERT INTO {table_name} (...) VALUES (...), (...), ...;` を組み立てる。1本のINSERT文に含める行数は `SQL_BATCH_SIZE`（1000）で区切っている。`sql_literal` が `resolve_value_category`（列タイプと`data_type`上書き指定から`'...'`で囲むかどうかを判定する。上記「列ごとのデータ型上書き指定」参照）を使い、値のクォート・エスケープ（`'` → `''`）を行う。
- **並列化の実測メモ**: 10列×100万行で計測したところ、値生成そのものは逐次実行で約1.55秒、rayon並列（8コア環境）で約0.55秒（約2.8倍）。ただし全体の処理時間は `std::fs::write` によるディスクへの実書き込みが約1〜1.4秒かかり支配的なため、体感の総時間短縮効果は限定的（並列化前後で3.155秒→2.6〜2.9秒程度）。CPUバウンドな部分は明確に速くなったが、ボトルネックは既にディスクI/O側に移っている、という計測結果に基づく判断。**xlsx出力はセル単位の書き込みのため大量データで特に重く、同じ10列×100万行のxlsx単体で約35秒かかる**（CSV/SQL/JSONの1桁上）。複数形式同時出力でxlsxを含めると、その分だけ全体時間が伸びる点に注意。
- `build_json_from_rows` は `serde_json::Map`（`preserve_order` featureで列の順序を保持）を使い、1行1件のNDJSON（`{"col":val, ...}\n{"col":val, ...}\n...`）を組み立てる。`cell_to_json` が列タイプに応じてJSONの型（数値/真偽値/文字列/null）を決める。
- **Excel出力（F2-2）**: `write_xlsx_from_rows`が`rust_xlsxwriter::Workbook`を直接組み立てて保存する。CSV/SQL/JSONは「文字列を組み立ててから`write_text`で書き出す」という流れだが、xlsxはテキストではなくバイナリ(ZIP)形式なのでこの流れには乗らず、単体で完結している。そのため**`--encoding`はxlsxには効かない**（`main`でSJIS指定時に警告を出す）。`write_xlsx_cell`が列タイプに応じて`write_number`/`write_boolean`/`write_string`を使い分け、NULL(None)は何も書かず空セルのままにする。
- `write_text` が UTF-8文字列を受け取り、`encoding` に応じてそのまま保存するか `encoding_rs::SHIFT_JIS.encode()` でShift-JISに変換してから保存する共通処理（CSV/SQL/JSONで使う。xlsxは対象外）。
- **複数形式同時出力（F3-3）**: `Args.format`は`Vec<Format>`で、`value_delimiter = ','`により`--format csv,json`のようなカンマ区切り指定を自動でパースする（`Format`に`PartialEq`/`Eq`/`Hash`を追加し、`main`側で重複指定を除去）。`format_extension`/`default_output_path`/`output_base_path`が出力パスを決める。**形式が1つだけのときは既存の挙動を一切変えない**（`--output`をそのまま使う）。**形式が複数のときは`--output`を拡張子なしのベース名として扱い**、形式ごとに拡張子を付ける（例: `--output result --format csv,json` → `result.csv`/`result.json`）。`write_output`が形式ごとの分岐（`sql`なら`table_name`必須チェックを含む）を一手に引き受ける。`sql` を選んだ場合、`schema.yaml` に `table_name` の指定がないとエラーになるが、**他に指定した形式の出力は続行する**（1つの形式の失敗が他をブロックしない）。
- エラーハンドリングは `Box<dyn std::error::Error>` + `?` によるシンプルな伝播で、`main` 側で単一テーブルなら `load_schema` → `prepare_columns` → `resolve_unique_pools` → `generate_all_rows`(1回) → 指定された各形式について`write_output`、複数テーブルなら下記の手順の順に処理し、形式ごとに成功/失敗を個別に表示する。

### 複数テーブルの外部キー整合性（F4-3）

最も規模が大きい機能。トップレベルに`tables:`（`Vec<Schema>`）を書くと複数テーブルを1つのschema.yamlにまとめられ、子テーブルの`foreign_key`列は親テーブルに実在する値だけを参照する。**既存の単一テーブル形式（`tables:`を使わない書き方）は`load_schema`の正規化を経て`SchemaFile{multi_table: false}`になり、`main`はそれ専用の分岐（既存コードと全く同じ手順）を通るため、出力は1バイトも変わらない**（実測で確認済み: リファクタ前後で同一seed・同一schema.yamlのCSVがバイト単位で一致）。

- `PreparedColumnType::ForeignKey { ref_table, ref_column, repr, pool }`: `pool`は親テーブル生成後に埋まる`Arc<Vec<String>>`（`unique_pool`と同じ「先に確定した値を使う」設計。`Arc`なのは同じ親列を複数の子列が参照しても実体を共有するため）。`repr: FkRepr`（`Integer`/`Float`/`Boolean`/`Text`）が参照先の列タイプから決まり、`default_value_category`経由でSQLのクォート要否やJSON/Excelの型（`cell_to_json`/`write_xlsx_cell`）を左右する。`references: "テーブル名.列名"`は`parse_reference`が最初の`.`で分割する。
- `PreparedTable { name, row_count, columns }`が「1テーブル分の生成準備が済んだ状態」。`prepare_tables`が`SchemaFile`の各テーブルに`prepare_columns`を適用する（単一テーブル形式で`foreign_key`列が使われていたら`reject_foreign_key_in_single_table`でエラーにする）。**この関数は`prepare_tables`以外に、GUI/HTTPサーバーの単一テーブル専用コマンド(`preview_dummy_data`/`generate_dummy_data`、src-serverの対応するハンドラ)からも直接呼ぶ**（これらは`prepare_tables`を経由せず`prepare_columns`を直接呼ぶため、元々`prepare_tables`にしか無かったこの検証が素通りしていた不具合があった。単一テーブルの`foreign_key`列は`fill_foreign_key_pools`(複数テーブル専用の経路でしか呼ばれない)が一度も実行されず、生成時に「FKプールは必ず埋まっている」というexpect()が必ずpanicし、Tauriのコマンド境界(WebView2のコールバック内)でpanicするとアプリ全体がクラッシュしていた。加えて`generate_dummy_data`/`generate_dummy_data_multi`/`preview_dummy_data`/`preview_dummy_data_multi`は`catch_panic_as_err`(dummygen_jp_gui/src-tauri側)で包み、想定外のpanicが今後起きてもアプリを道連れにしないようにしてある）。
- `resolve_foreign_keys`が全FK列の参照先（テーブル/列の存在、自己参照、参照先の`null_rate`）を検証し、依存辺（`deps[子index] = [親index...]`）と「後でプールを取り出す必要がある列」（`referenced: HashMap<ColumnKey, String>`、`ColumnKey = (String, String)`型エイリアス）を作る。
- `topological_order`がKahnのアルゴリズムで親→子の順に並べる。循環していたら具体的な循環パスを1つ再構成してエラーメッセージに含める。
- `resolve_fk_reprs`が**トポロジカル順**に走査して`repr`を確定させる（多段参照 `c.b_id → b.a_id → a.id` で、bのreprが確定してからcのreprを決めるため。順不同だと多段参照時にreprが未確定の`Text`のままになってしまう）。
- `table_seed(base_seed, table_index)` = `base_seed.wrapping_add(table_index * 定数)`。`table_index`は**宣言順index**（トポロジカル順ではない）を使うため、`table_index == 0`のとき必ず`base_seed`そのものになり、単一テーブルの再現性に影響しない。同じ列構成の2テーブルが同一データにならないようにする目的。
- **`generate_multi_table_rows(tables, order, referenced, base_seed, on_table_start)`**: `order`（親→子）に従い、各テーブルごとに `fill_foreign_key_pools`（既に確定した親のプールをFK列に差し込む）→ `resolve_unique_pools` → `generate_all_rows` → `collect_key_pools`（このテーブルの、他から参照される列の値をプール化する）を実行し、`rows_by_table: Vec<Option<Vec<Vec<Option<String>>>>>`（`&mut tables[i]`と`&tables[j]`の同時借用を避けるため`tables`とは別に持つ）を返す`pub`関数。元々`main`関数の中に直接書かれていたループを、`dummygen_jp_gui`のTauriコマンド（`generate_dummy_data_multi`/`preview_dummy_data_multi`）からも呼べるよう切り出したもの（ロジックは変更していない。`on_table_start: impl FnMut(usize, &PreparedTable)`は「今どのテーブルの生成を始めたか」を呼び出し側に知らせるコールバックで、CLIは`eprintln!`、GUIはTauriの進捗イベント発火に使う）。`main`はこれを呼ぶだけの薄い形になっている。
- `GeneratedTable<'a> { name, columns, rows }`（すべて借用）が出力用のビュー。`write_output_multi_table`が形式ごとに分岐する: csv/json は`table_file_path`（`"result.csv"+"users"`→`"result_users.csv"`、書き込み前にサニタイズ後の衝突を検査）でテーブルごとに別ファイル、sql は`build_sql_multi`で依存順（親→子）の`INSERT INTO`を1ファイルにまとめる（外部キー制約のあるDBにそのまま流し込める）、xlsx は`write_xlsx_tables`で1ブック・テーブルごとに1シート（`sanitize_sheet_name`が31文字制限・禁止文字・重複に対応）。`write_xlsx_from_rows`（単一テーブル）は`write_xlsx_tables`にシート名指定なしで委譲する薄いラッパー。

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

## GUI（DummyGen JP）との関係

`C:\dev_2\dummygen_jp_gui` に、このエンジンを土台にしたTauri v2 + React 19のGUIデスクトップアプリ「DummyGen JP」と、同じエンジンをブラウザ経由で使うためのHTTPサーバー（`src-server`、axum使用）がある（どちらも`dummy_data_gen`を`path`依存として参照）。GUIはCSV/SQL/Excel(xlsx)出力に対応する（JSON出力のみCLI専用のまま。xlsxは単一テーブルなら`generate_all_rows`+`write_xlsx_from_rows`、複数テーブルなら`write_output_multi_table`のXlsx分岐＝`write_xlsx_tables`を使い、ストリーミング書き込みは無い点はCLIと同じ）。単一テーブルのCSV/SQLは`prepare_columns`/`resolve_unique_pools`/`write_csv_streaming`/`write_sql_streaming`をTauriコマンド(またはHTTPハンドラ)から直接呼び出す（今まで通りストリーミングでメモリを圧迫しない）。複数テーブル（外部キー）のときは`prepare_tables`/`resolve_foreign_keys`/`topological_order`/`resolve_fk_reprs`/`generate_multi_table_rows`/`write_output_multi_table`を使う専用のコマンド/ハンドラ（`generate_dummy_data_multi`/`preview_dummy_data_multi`、src-serverでは`/api/generate_multi`等）を別に用意している。`generate_multi_table_rows`はCLI（`main.rs`）の複数テーブル生成ループを切り出した共通関数で、CLI・GUI双方から呼ばれる。複数テーブルは`generate_all_rows`で全行をメモリに載せてから書き出す方式のみ（単一テーブル用のストリーミング書き込みに複数テーブル版は無い）。また、`schema_file_to_yaml`（`Schema`/`ColumnDef`/`ColumnType`に付けた`Serialize`を使う）がGUIの「列設定をYAMLとして保存」機能から呼ばれる（`load_schema`の逆方向）。この`dummy_data_gen`側に列タイプを追加・変更した場合、GUI側の`src/types.ts`（TypeScriptの型定義・列タイプ一覧）も手動で同期する必要がある（自動生成ではないため）。
