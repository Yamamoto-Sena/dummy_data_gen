# dummy_data_gen

オフライン専用のダミーデータ(CSV/SQL/JSON/Excel)ジェネレーター。Rust製のCLIツールで、外部のWebサービスを一切使わずに、ローカル環境だけでテスト用のダミーデータを高速に生成する。

## 特徴

- **高速**: 値の生成をRustのマルチスレッド処理(rayon)で並列化。10列×100万行でも数秒で生成できる。
- **完全オフライン**: ネットワーク通信を一切行わない。テストデータ(氏名・メールアドレス等)を外部サービスに送信できない、社内のセキュリティ規定にも対応しやすい。
- **YAMLで自由にスキーマ定義**: 列名・型・行数を`schema.yaml`に書くだけ。Gitで管理・レビューできる。
- **CSV/SQL/JSON/Excelに対応**: `--format csv`(デフォルト)/ `--format sql`(`INSERT INTO`文をバッチ生成)/ `--format json`(NDJSON、1行1件。`--json-array`で配列形式)/ `--format xlsx`(Excelファイル)。
- **Shift-JIS出力に対応**: `--encoding sjis`。Dr.Sum/MotionBoard/Datalizerなど、日本の業務システムはUTF-8だけでなくShift-JISを前提にしていることが多いため。
- **再現性**: `--seed`で乱数シードを固定すると、同じ設定から毎回同じデータを再現できる。
- **NULL値の混入**: 列ごとに`null_rate`(0.0〜1.0)を指定すると、指定した確率でNULL(空)を混ぜられる。
- **enum型列**: `choices`のリストからランダムに1つ選ぶ列を定義できる(会員ステータスなど)。
- **unique制約**: 列ごとに`unique: true`を指定すると、行間で値が重複しないようにできる。
- **都道府県⇔市区町村の整合性**: `prefecture_ja`列の後に`city_ja`列を置くと、その都道府県に実在する市区町村が選ばれる。
- **進捗表示**: 1000行以上のデータを生成するとき、ターミナルに進捗バーを表示する。

## インストール・ビルド

[Rust](https://www.rust-lang.org/ja/tools/install)(`rustup`)がインストールされていれば、追加のセットアップは不要。

```bash
cargo build --release
```

生成される実行ファイルは `target/release/dummy_data_gen`(Windowsは `.exe`)。

## 使い方

### 1. スキーマファイル(schema.yaml)を書く

```yaml
row_count: 20
table_name: users # --format sql のときにINSERT文のテーブル名として使う
columns:
  - name: id
    type: sequence
  - name: name
    type: name_ja
  - name: email
    type: email
  - name: age
    type: integer
    min: 18
    max: 65
    unique: true # 行間で値が重複しないようにする
  - name: status
    type: enum
    choices: [利用中, 休止中, 退会済み]
  - name: phone
    type: phone_ja
    null_rate: 0.1 # 10%の確率でNULL(未入力)にする
```

### 2. 実行する

```bash
cargo run -- --config schema.yaml --format csv --encoding utf8
```

デフォルトでは形式に応じて `output.csv` / `output.sql` / `output.json` / `output.xlsx` に上書き保存される。`--output <path>` で保存先を明示的に指定することもできる。

### CLIオプション

| オプション | 説明 | デフォルト |
|---|---|---|
| `--config <path>` | スキーマファイルのパス | `schema.yaml` |
| `--format <csv\|sql\|json\|xlsx>` | 出力形式。カンマ区切りで複数指定可(例: `csv,json`)。`sql`は`table_name`の指定が必須。`xlsx`では`--encoding`は無視される | `csv` |
| `--encoding <utf8\|sjis>` | 出力の文字コード | `utf8` |
| `--seed <数値>` | 乱数シード。指定すると毎回同じデータを再現する | 未指定(毎回ランダム) |
| `--output <path>` | 出力先ファイルパス | 形式に応じた既定値 |
| `--quote-all` | CSV出力で全ての値をダブルクォートで囲む(名称にスペースを含む値の区切りを明確にしたい場合向け)。sql/json/xlsxには影響しない。単一テーブル(`tables:`を使わない形式)では`--format csv`単体のときのみ有効。複数テーブル(`tables:`形式)ではcsvを含む出力であれば常に有効(他の形式と同時指定してもcsvの部分にだけ効く) | 指定なし(必要な値だけクォート) |
| `--json-array` | JSON出力を、ファイル全体で1つのJSON配列(`[{...},{...}]`)にする。csv/sql/xlsxには影響しない。複数テーブルではテーブルごとのファイルがそれぞれ1つの配列になる | 指定なし(NDJSON、1行1件) |

### 対応する列タイプ

| type | 説明 | 追加パラメータ |
|---|---|---|
| `sequence` | 1から始まる連番 | なし |
| `name_ja` | 日本語のランダムな氏名(フルネーム) | `with_space`(省略時false。trueで姓と名の間にスペース、例:「山田 太郎」) |
| `last_name_ja` | 姓(苗字)のみ | なし |
| `first_name_ja` | 名のみ | なし |
| `romaji_name` | 氏名のローマ字(ヘボン式、「姓 名」の順、例: `Yamada Taro`)。**直前に`name_ja`列があれば、その氏名と対応するローマ字を選ぶ**。無ければランダム | なし |
| `katakana_last_name` | フリガナ(姓)のみ。他の列とは対応付けない独立ランダム | なし |
| `katakana_first_name` | フリガナ(名)のみ。他の列とは対応付けない独立ランダム | なし |
| `katakana_name` | フリガナ(全角)。**直前に`name_ja`列があれば、その氏名と対応する読みを選ぶ**。無ければランダム | `with_space`(省略時false。trueで姓の読みと名の読みの間にスペース、例:「ヤマダ タロウ」) |
| `katakana_name_hankaku` | フリガナ(半角)。参照ロジックは`katakana_name`と同じ | `with_space`(省略時false。意味は`katakana_name`と同じ) |
| `gender` | "男性"/"女性"を50%ずつのランダムで返す。他の列との連動は無い独立ランダム | なし |
| `blood_type` | "A型"/"O型"/"B型"/"AB型"を、日本人の血液型分布の目安(4:3:2:1)で重み付けして返す | `with_suffix`(省略時true。falseで「型」を付けずに「A」「O」「B」「AB」を返す) |
| `email` | `user{連番}@{domain}` | `domain`(省略時`example.com`) |
| `integer` | `min`〜`max`のランダムな整数 | `min`, `max` |
| `float` | `min`〜`max`のランダムな小数 | `min`, `max`, `decimals`(省略時2) |
| `boolean` | `true` / `false` | なし |
| `date` | `start`〜`end`のランダムな日付 | `start`, `end`, `format`(省略時`ymd`。`ymd`/`iso8601`/`slash`/`compact`(区切りなし`YYYYMMDD`)/`wareki`) |
| `birth_date` | `min_age`〜`max_age`歳になる生年月日を、今日の日付から逆算 | `min_age`, `max_age`, `format`(dateと共通) |
| `postal_code` | 日本の郵便番号風(`NNN-NNNN`) | `with_hyphen`(省略時true。falseで「-」を入れず`NNNNNNN`にする) |
| `phone_ja` | 携帯電話番号風(`090/080/070-XXXX-XXXX`) | `with_hyphen`(省略時true。falseで「-」を入れない) |
| `phone_ja_landline` | 固定電話番号風(市外局番+`XXXX-XXXX`) | `with_hyphen`(省略時true。falseで「-」を入れない) |
| `address_ja` | 都道府県+市区町村の簡易住所 | `allowed_prefectures`(省略可。指定した都道府県名のリストからだけ選ぶ。例: `["東京都", "大阪府"]`。省略時は47都道府県すべて) |
| `company_name_ja` | 「株式会社〇〇商事」のような会社名 | なし |
| `uuid` | UUID v4形式のランダムなID(例: `550e8400-...`) | なし |
| `prefecture_ja` | 都道府県 | `allowed_prefectures`(省略可。`address_ja`と同じ意味) |
| `city_ja` | 市区町村。**直前に`prefecture_ja`列があれば、その都道府県に実在する地名を選ぶ**。無ければ全都道府県からランダム | なし |
| `department_ja` | 部署名(「営業部」など) | なし |
| `job_title_ja` | 役職名(「課長」など) | なし |
| `ip_address` | IPv4アドレス(例: `192.168.1.1`) | なし |
| `jwt` | それっぽい形のJWT風文字列(署名検証はできないダミー) | なし |
| `api_key` | `sk_`+英数字32文字のAPIキー風文字列 | なし |
| `username` | ローマ字の名前(小文字)+3桁の数字(例: `taro123`) | なし |
| `password` | 英大文字・英小文字・数字・一部記号を含むダミーのパスワード風文字列(12文字) | なし |
| `profile_image_url` | プレースホルダー画像サービス(`placehold.jp`)のURL文字列。実際に画像を取得したり通信したりはしない | なし |
| `credit_card_number` | 16桁、Luhnアルゴリズムで検査数字を計算したクレジットカード番号風 | なし |
| `credit_card_expiry` | クレジットカードの有効期限(`MM/YY`形式、今日から1〜5年後) | `with_slash`(省略時true。falseで「/」を入れず`MMYY`にする) |
| `bank_account_number` | 日本の普通預金口座番号を想定した7桁のゼロ埋め数字 | なし |
| `product_sku` | `SKU-`+英大文字/数字8文字の商品SKU風文字列 | なし |
| `my_number` | 12桁、個人番号法の検査数字計算方法に従ったマイナンバー風 | なし |
| `enum` | `choices`の中からランダムに1つ選ぶ | `choices`(文字列のリスト)、`weights`(省略可。`choices`と同じ個数の数値のリストで出現比率を指定する。例: `choices: [利用中, 休止中]` に `weights: [7, 3]` で7:3の比率。省略時は均等ランダム)。`choices`に同じ値を複数回書いた場合はエラーにならず、その分だけ出現しやすくなる(例: `choices: [A, A, B]` はA:Bがおよそ2:1になる。`weights`を使わずに簡易的に比率を偏らせたい場合に使えるが、意図せず重複させてしまうと気づきにくいので注意) |
| `fixed` | どの行でも常に同じ文字列を返す | `value` |
| `pattern` | 正規表現に似た簡易記法から値を生成する(例: `"[A-Z]{3}-[0-9]{4}"` → `"ABC-1234"`)。対応するのはリテラル文字・`[...]`文字クラス(範囲`a-z`・`^`否定)・量指定子`?`/`*`/`+`/`{n}`/`{n,}`/`{n,m}`のみで、グループ化`(...)`や選択`\|`を使うとエラーになる(その文字自体を値に含めたい場合は`\(`のように`\`でエスケープする) | `pattern` |
| `foreign_key` | 親テーブルに実在する値からランダムに1つ選ぶ(複数テーブル形式`tables:`専用) | `references`(`"テーブル名.列名"`形式) |
| `correlated_number` | 他の列の値から計算する数値(例: 売上金額 = 数量 × 単価)。この列より**前**に定義された列だけを参照できる | `base_columns`(掛け合わせる数値列名のリスト。`integer`/`float`/`sequence`/`correlated_number`のみ指定可、1つ以上必須)。`category_column`+`category_multipliers`(省略可。指定した列の実際の値ごとに倍率を変える。例: `category_column: category`、`category_multipliers: {食品: 0.8, 家電: 3.0}`。一致しない値は倍率1.0のまま)。`date_column`+`monthly_multipliers`(省略可。指定した`date`/`birth_date`列の月によって倍率を変える。12個の数値、1月始まり)。`noise`(省略可・既定0.0。`±noise`のランダムな乱数を最後に掛ける。例: 0.1で±10%)。`decimals`(省略可・既定0)。`min`/`max`(省略可。結果をこの範囲でクランプする) |
| `tax_amount` | 消費税額(税抜金額 × 税率を端数処理した整数)。詳細は下の「消費税額・税込金額」参照 | `base_column`(税抜金額の列名、必須)、`tax_rate`(省略時0.10)、`category_column`+`category_rates`(省略可。区分ごとの税率)、`rounding`(`floor`/`round`/`ceil`、省略時`floor`) |
| `tax_inclusive_amount` | 税込金額(税抜金額 + 消費税額) | `tax_amount`と同じ |

どの列タイプにも `null_rate`(0.0〜1.0)を追加でき、その確率でNULL(CSVでは空文字、SQLではクォートなしの`NULL`、JSONでは`null`)を出力する。

どの列タイプにも `data_type`(文字列、省略可)を追加できる。これは、SQL/JSON/Excel出力でその列の値を文字列としてクォートするか、整数/小数/真偽値として出すかを、列タイプからの自動判定から上書きするための指定。案件によって「この列は本当はこの型として扱ってほしい」という決まりがある場合に使う(例: `postal_code`を`with_hyphen: false`にしたうえで`data_type: "INTEGER"`にすると、SQLでクォート無し・JSONで数値として出力される。逆に`integer`列に`data_type: "VARCHAR(10)"`を指定すると、0埋めのコードのような数値に見える値を文字列として保持できる)。`INT`/`INTEGER`/`BIGINT`/`SMALLINT`/`TINYINT`は整数、`FLOAT`/`DOUBLE`/`DECIMAL`/`NUMERIC`/`REAL`は小数、`BOOL`/`BOOLEAN`/`BIT`は真偽値と判定し、それ以外(`VARCHAR`/`CHAR`/`TEXT`/`DATE`など、知らない型名も含む)は文字列として扱う(先頭の単語だけを大文字化して判定するため、`VARCHAR(100)`のように括弧付きで書いてもよい)。省略時は今まで通り列タイプから自動判定する。CSV出力には影響しない(CSVには元々型の区別が無いため)。実際の値の見た目と違う型を指定すると、SQL/JSONの出力が壊れた形になることがあるので注意。

生成される氏名・住所・電話番号・郵便番号・会社名・カード番号・口座番号等はすべて固定リストや簡易な計算式からのランダムな組み合わせで、実在する個人・企業・住所・番号とは一切関係がない。

### unique制約

列に `unique: true` を付けると、その列の値が行間で重複しないようにする。対応している型は以下の通り。

- 組み合わせ数が少なく全列挙できる型: `enum` / `boolean` / `gender`(2通り) / `blood_type`(4通り) / `integer` / `date` / `birth_date`(dateと同じ日数分) / `name_ja`(姓30×名20=600通り) / `last_name_ja`(30通り) / `first_name_ja`(20通り) / `romaji_name`(600通り) / `katakana_last_name`(30通り) / `katakana_first_name`(20通り) / `prefecture_ja`(都道府県の登録数通り) / `company_name_ja`(姓30×会社の種類8=240通り) / `department_ja`(18通り) / `job_title_ja`(17通り) / `credit_card_expiry`(1〜5年後×1〜12月=60通り) / `fixed`(常に同じ値なので組み合わせは1通り。`row_count: 1`のときだけunique指定が成立し、2行以上を指定すると他の型と同じ「組み合わせが足りない」エラーになる)(`enum`に`weights`を指定していても、`unique: true`のときは全選択肢を重複なく列挙するだけなので`weights`は無視される)
- 組み合わせ数が膨大(だが十分すぎるほど大きい)ため、値を作っては重複チェックし被ったら作り直す方式で対応する型: `phone_ja` / `phone_ja_landline` / `postal_code` / `address_ja` / `uuid` / `ip_address` / `jwt` / `api_key` / `username` / `password` / `profile_image_url` / `credit_card_number` / `bank_account_number` / `product_sku` / `my_number`
- `float`は`min`/`max`/`decimals`(小数点以下の桁数)から組み合わせ数を自動計算し、200万通り以下なら上の「全列挙」方式、それより多ければ「作っては重複チェック」方式のどちらかに自動的に振り分けられる(例: `min: 0, max: 1, decimals: 1`なら11通りで全列挙、`min: 0, max: 100万, decimals: 2`のような広い範囲なら重複チェック方式になる)
- `sequence` / `email` はそもそも仕組み上値が絶対に重複しない(`sequence`は連番そのもの、`email`は`user{連番}@ドメイン`)ため、`unique`を付ける必要が無く、`unique`自体に対応していない
- それ以外(`pattern`(組み合わせ数の計算が複雑)/`city_ja`・`katakana_name`・`katakana_name_hankaku`(直前の列を参照して値を選ぶため`unique`との組み合わせが複雑)/`foreign_key`(1つの親の値を複数の子行が参照するのが外部キーの通常の挙動のため))は非対応。値の重複を避けたい場合は、より小さい組み合わせ数の`enum`/`integer`で代用することを想定している

以下の場合はエラーで停止する。

- 値の組み合わせ数が `row_count` より少ない(例: `boolean`で行数5行にunique指定)
- 全列挙方式の型で、値の組み合わせ数が200万通りを超える(範囲を絞って使う)
- `null_rate` と同時に指定している

### 複数テーブルの外部キー整合性

トップレベルに `tables:` を書くと、複数のテーブルをまとめて定義できる。子テーブルの `foreign_key` 列は、必ず親テーブルに実在する値だけを参照する([schema_multi_table.yaml](schema_multi_table.yaml)参照)。

```yaml
tables:
  - name: users
    row_count: 50
    columns:
      - name: id
        type: sequence
  - name: orders
    row_count: 200
    columns:
      - name: id
        type: sequence
      - name: user_id
        type: foreign_key
        references: users.id # "テーブル名.列名" の形式
```

```bash
cargo run -- --config schema_multi_table.yaml --format csv,sql
```

- **既存の単一テーブル形式(`tables:`を使わない書き方)は今まで通りそのまま動く**。`tables:`と`row_count:`/`columns:`/`table_name:`を同時に指定するとエラーになる。
- テーブルは依存関係を自動で解決し、親テーブルを先に生成してから子テーブルを生成する(`tables:`に書く順序は自由。多段参照・複数のテーブルから参照される親も対応)。循環参照・自己参照・存在しないテーブルや列への参照はエラーになる。
- `foreign_key`列に `unique: true` は指定できない(1つの親の値を複数の子行が参照するのが外部キーの通常の挙動のため)。参照先の列に `null_rate` を指定することもできない(参照先にNULLが混ざるのを防ぐため)。参照する側の列自体への`null_rate`は指定できる(任意の関連を表せる)。
- 出力先: `csv`/`json`はテーブルごとに別ファイル(`output_users.csv`など)、`sql`は依存順(親→子)で1つのファイルにまとめる、`xlsx`は1ブックにテーブルごとの1シートとしてまとめる。

### JSON出力(NDJSON)

`--format json` を指定すると、1行1件のJSONオブジェクトを改行区切りで出力する(NDJSON形式)。

```json
{"id":1,"name":"佐藤翔太","age":32,"status":"利用中"}
{"id":2,"name":"田中健太","age":29,"status":"休止中"}
```

数値・真偽値の列はJSON上でも数値・真偽値として出力され(クォートされない)、NULLは`null`になる。

ファイル全体を1つのJSONとして読み込むツール向けに、`--json-array` を付けると配列形式で出力する(値の中身はNDJSONと同じ)。

```bash
cargo run -- --config schema.yaml --format json --json-array --output result.json
```

```json
[
  {"id":1,"name":"佐藤翔太","age":32,"status":"利用中"},
  {"id":2,"name":"田中健太","age":29,"status":"休止中"}
]
```

### Excel(xlsx)出力

`--format xlsx` を指定すると、Excelでそのまま開けるファイルを出力する。数値・真偽値の列はExcel上でも数値・真偽値のセルになり(文字列として入らない)、NULLは空セルになる。`--encoding`はxlsxには意味を持たない(Excelファイルは内部的に常にUTF-8相当のため)。

```bash
cargo run -- --config schema.yaml --format xlsx --output result.xlsx
```

### 複数形式の同時出力

`--format`にカンマ区切りで複数指定すると、1回の実行で全部の形式を出力する(値の生成は1回だけ行い、使い回すので効率的)。

```bash
cargo run -- --config schema.yaml --format csv,json,xlsx
```

- 形式を1つだけ指定したときは、これまで通り`--output`をそのまま出力先パスとして使う。
- 形式を複数指定したときは、`--output`を「拡張子なしのベース名」として扱い、形式ごとに拡張子を付ける。

```bash
cargo run -- --config schema.yaml --format csv,json --output result
# → result.csv と result.json が生成される
```

`--format sql`を他の形式と同時指定して`table_name`が未設定の場合、sqlだけがエラーになり、他の形式の出力は続行される。

### 都道府県⇔市区町村の整合性

`prefecture_ja`列の**後ろ**に`city_ja`列を書くと、その行の都道府県に実在する市区町村が選ばれる(「東京都なのに市区町村は北海道の地名」のような不自然な組み合わせにならない)。

```yaml
columns:
  - name: prefecture
    type: prefecture_ja
  - name: city
    type: city_ja # prefectureより後ろに書く
```

`city_ja`だけを単独で使った場合は、全都道府県の市区町村からランダムに選ぶ。`prefecture_ja`列は定義してあるのに`city_ja`より**後ろ**にある場合(順序を間違えた場合)は、対応関係が保てないため実行時に警告が表示される。

`prefecture_ja`/`city_ja`/`address_ja`は47都道府県すべてに対応しており、各都道府県につき5件ずつ実在する市区町村名(政令指定都市の区・県庁所在地など)を収録している。

`prefecture_ja`/`address_ja`は`allowed_prefectures`(都道府県名のリスト)を指定すると、その都道府県だけからランダムに選ぶように絞り込める(省略時は今まで通り47都道府県すべてが対象)。

```yaml
columns:
  - name: prefecture
    type: prefecture_ja
    allowed_prefectures: ["東京都", "大阪府"] # この2つだけから選ぶ
```

`allowed_prefectures`で絞り込んだ`prefecture_ja`列の後ろに`city_ja`列を置いた場合も、その行で実際に選ばれた都道府県(絞り込んだ範囲内のどれか)に対応する市区町村が選ばれる(`city_ja`自体に絞り込みの指定はできないが、前の`prefecture_ja`列の絞り込みに自動で追従する)。

### 氏名の整合性

`name_ja`列の**後ろ**に`katakana_name`列を書くと、その行の氏名と対応するフリガナが選ばれる(都道府県⇔市区町村と同じ考え方)。

```yaml
columns:
  - name: name
    type: name_ja
  - name: kana_name
    type: katakana_name # nameより後ろに書く
```

`katakana_name`だけを単独で使った場合はランダムなフリガナを選ぶ。`name_ja`列は定義してあるのに`katakana_name`より後ろにある場合は、都道府県⇔市区町村のときと同様に警告が表示される。`romaji_name`(氏名のローマ字)も`katakana_name`と全く同じ参照ロジックを持つ(`name_ja`より後ろに置くと対応するローマ字になる)。

### 相関のある数値(correlated_number)

`integer`/`float`は完全にランダムな数値だが、`correlated_number`は他の列の値から計算する数値。**参照する列は、この列より前に定義しておく必要がある**(都道府県⇔市区町村・氏名の整合性と同じ考え方だが、こちらは警告ではなく必須の制約で、違反すると生成前にエラーになる)。

```yaml
columns:
  - name: quantity
    type: integer
    min: 1
    max: 5
  - name: unit_price
    type: integer
    min: 100
    max: 1000
  - name: category
    type: enum
    choices: [食品, 家電]
  - name: purchase_date
    type: date
    start: "2024-01-01"
    end: "2024-12-31"
  - name: sales_amount
    type: correlated_number
    base_columns: [quantity, unit_price] # quantity × unit_priceを基準値にする
    category_column: category            # カテゴリによって価格帯を変える
    category_multipliers:
      食品: 0.8
      家電: 3.0
    date_column: purchase_date           # 月によって売上の傾向を変える(季節変動)
    monthly_multipliers: [1,1,1,1,1,1,1,1,1,1,1,2] # 12月だけ2倍
    noise: 0.1                           # ±10%のランダムなブレを加える
    decimals: 0
```

`base_columns`/`category_column`/`category_multipliers`/`date_column`/`monthly_multipliers`/`noise`はすべて省略可(必須は`base_columns`のみ)なので、必要な仕組みだけを組み合わせて使える(例: 計算式だけ使う、季節変動だけ使う、など)。SQL/JSON/Excel出力では常に数値として扱われる(クォートされない)。unique制約は非対応(計算結果に対して事前に全パターンを用意するのが現実的でないため、`pattern`/`foreign_key`と同じ扱い)。

### 消費税額・税込金額(tax_amount / tax_inclusive_amount)

純売上(税抜金額)の列から消費税額・税込金額を計算する列。税抜金額の列(`base_column`)は**この列より前**に定義する(`integer`/`float`/`sequence`/`correlated_number`のみ指定可。違反すると生成前にエラー)。純売上そのものは上の`correlated_number`(数量×単価など)で作る。

```yaml
columns:
  - name: category
    type: enum
    choices: [食品, その他]
  - name: net_sales                 # 純売上(税抜)
    type: correlated_number
    base_columns: [quantity, unit_price]
  - name: tax
    type: tax_amount                # 消費税額
    base_column: net_sales
    tax_rate: 0.10                  # 標準税率。10%なら0.10(省略時0.10)
    category_column: category       # 区分ごとに税率を変える(軽減税率など。省略可)
    category_rates:
      食品: 0.08                    # 一覧に無い区分は標準税率
    rounding: floor                 # 端数処理: floor(切り捨て・既定)/round(四捨五入)/ceil(切り上げ)
  - name: gross
    type: tax_inclusive_amount      # 税込金額 = 税抜金額 + 消費税額
    base_column: net_sales
    tax_rate: 0.10
    category_column: category
    category_rates:
      食品: 0.08
    rounding: floor
```

- `tax_amount`と`tax_inclusive_amount`は設定項目が共通。**税率の設定は列ごとに持つ**ため、税込金額を出すときは消費税額の列と同じ設定を書く(食い違うと税込金額が消費税額列と合わなくなる)。
- 税率は割合で指定する(10%なら`0.10`。`10`と書くとエラー)。範囲は0.0〜1.0。
- 消費税額は円未満を`rounding`で処理した整数。税込金額は税抜金額の小数桁数を引き継ぐ(整数なら整数)。1000×10%のような計算で出る浮動小数点の誤差(100.00000000000001)は端数処理の前に除去しているため、切り上げても101にならない。
- 税抜金額の列がNULL(`null_rate`)のときは、この列もNULLになる。
- SQL/JSON/Excel出力では`tax_amount`は整数、`tax_inclusive_amount`は小数として扱われる(`data_type`で上書き可)。unique制約は非対応。

### 進捗表示

`row_count`が1000以上のとき、生成中にターミナルへ進捗バー(件数・割合)を表示する。ファイルへのリダイレクトなど、ターミナル以外への出力時は自動的に表示されない。

## GUI版(DummyGen JP)について

`C:\dev_2\dummygen_jp_gui` に、このツールをライブラリ(`src/lib.rs`)として組み込んだTauri v2 + React 19のデスクトップGUIアプリ「DummyGen JP」がある。コマンド操作に不慣れな担当者向けに、カラム設定・生成件数・出力形式(CSV/SQL/JSON/Excel)・文字コード(UTF-8/Shift-JIS。Excel選択時は文字コードの概念が無いため設定不要)・値をダブルクォートで囲むかどうかを画面から設定できるほか、サンプルCSVの読み込みによる列・選択肢の自動設定、列のドラッグ並べ替え・複製、設定の保存・読み込み(プルダウン)、ワンクリックテンプレート、生成前のリアルタイムプレビュー(表示件数を変更可能)、複数テーブル(外部キー)対応、列設定のYAML書き出し/読み込み(このツールの`schema.yaml`と互換)といった画面独自の機能を持つ。GUIはCSV/SQL/JSON/Excel出力に対応(JSONはNDJSON/配列形式を選べ、文字コードは常にUTF-8。CLIと同じく全行を一度メモリに載せてから書き出す)。Excel(xlsx)出力は単一テーブル・複数テーブルのどちらも、CLIと同じく全行を一度メモリに載せてから書き出す方式(ストリーミング書き込みは無い)。複数テーブルのCSV/SQL生成も、CLI(`tables:`形式)と同じく全テーブル分の行を一度メモリに載せてから書き出す方式(単一テーブルのCSV/SQLは今まで通りメモリを圧迫しない方式のまま)。`quote_all`(値を""で囲むオプション)は単一テーブル・複数テーブルのどちらのCSV出力でも使える。GUIから生成したUTF-8のCSVには、Excelで開いたときに文字化けしないようファイル先頭にBOMを付けている(CLIの`--encoding utf8`出力にはBOMは付かず、これまで通り)。

## Dr.Sum / MotionBoard / Datalizer 等での利用について

これらの日本製BI/レポーティングツールはCSVの取り込み・出力の両方で文字コード(UTF-8/Shift-JIS)を指定できる。取り込み先の設定に合わせて `--encoding` を選ぶと文字化けを防げる。SQL(`--format sql`)は`INSERT INTO`文なので、Dr.SumのようにSQL実行機能を持つDB製品には直接流し込むことも可能。

## テスト

```bash
cargo test
```

バリデーション(min>maxの検出、不正な日付形式、null_rate/uniqueの範囲チェックなど)、SQLエスケープ、CSV/JSON/ExcelでのNULL表現、都道府県⇔市区町村の整合性、unique制約の重複なし生成、UUID形式の妥当性などを中心にユニットテストでカバーしている。

## パフォーマンスの実測値

10列×100万行のCSV生成(8コア環境)で計測:

- 値生成(rayonで並列化): 約0.55秒(逐次実行では約1.55秒)
- ディスクへの書き込み: 約1〜1.4秒(こちらがボトルネック。OS/ディスク側の制約でありCPUの並列化では改善できない)

`--format`に複数形式を指定した場合、値生成(上記)は1回だけ行い、形式ごとに清書処理を
繰り返すだけなので、形式を増やしても生成コストは重複しない。ただし清書・書き込み自体の
コストは形式ごとに異なる。同じ10列×100万行で比較すると、CSV+JSON同時出力は約8秒
(CSVだけなら約3秒)。**xlsx出力はセル単位で書き込む形式の特性上、大量データでは
特に重く、10列×100万行のxlsx単体で約35秒かかる**(CSV/SQL/JSONとは1桁違う)。
xlsxで大量データを扱う予定がある場合は注意すること。
