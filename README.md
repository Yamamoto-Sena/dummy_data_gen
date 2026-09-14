# dummy_data_gen

オフライン専用のダミーデータ(CSV/SQL)ジェネレーター。Rust製のCLIツールで、外部のWebサービスを一切使わずに、ローカル環境だけでテスト用のダミーデータを高速に生成する。

## 特徴

- **高速**: 値の生成をRustのマルチスレッド処理(rayon)で並列化。10列×100万行でも数秒で生成できる。
- **完全オフライン**: ネットワーク通信を一切行わない。テストデータ(氏名・メールアドレス等)を外部サービスに送信できない、社内のセキュリティ規定にも対応しやすい。
- **YAMLで自由にスキーマ定義**: 列名・型・行数を`schema.yaml`に書くだけ。Gitで管理・レビューできる。
- **CSV/SQLの両方に対応**: `--format csv`(デフォルト)/ `--format sql`(`INSERT INTO`文をバッチ生成)。
- **Shift-JIS出力に対応**: `--encoding sjis`。Dr.Sum/MotionBoard/Datalizerなど、日本の業務システムはUTF-8だけでなくShift-JISを前提にしていることが多いため。
- **再現性**: `--seed`で乱数シードを固定すると、同じ設定から毎回同じデータを再現できる。
- **NULL値の混入**: 列ごとに`null_rate`(0.0〜1.0)を指定すると、指定した確率でNULL(空)を混ぜられる。

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
  - name: phone
    type: phone_ja
    null_rate: 0.1 # 10%の確率でNULL(未入力)にする
```

### 2. 実行する

```bash
cargo run -- --config schema.yaml --format csv --encoding utf8
```

デフォルトでは `output.csv`(または `--format sql` のとき `output.sql`)に上書き保存される。

### CLIオプション

| オプション | 説明 | デフォルト |
|---|---|---|
| `--config <path>` | スキーマファイルのパス | `schema.yaml` |
| `--format <csv\|sql>` | 出力形式。`sql`は`table_name`の指定が必須 | `csv` |
| `--encoding <utf8\|sjis>` | 出力の文字コード | `utf8` |
| `--seed <数値>` | 乱数シード。指定すると毎回同じデータを再現する | 未指定(毎回ランダム) |

### 対応する列タイプ

| type | 説明 | 追加パラメータ |
|---|---|---|
| `sequence` | 1から始まる連番 | なし |
| `name_ja` | 日本語のランダムな氏名 | なし |
| `email` | `user{連番}@example.com` | なし |
| `integer` | `min`〜`max`のランダムな整数 | `min`, `max` |
| `float` | `min`〜`max`のランダムな小数 | `min`, `max`, `decimals`(省略時2) |
| `boolean` | `true` / `false` | なし |
| `date` | `start`〜`end`のランダムな日付(`YYYY-MM-DD`) | `start`, `end` |
| `postal_code` | 日本の郵便番号風(`NNN-NNNN`) | なし |
| `phone_ja` | 携帯電話番号風(`090/080/070-XXXX-XXXX`) | なし |
| `address_ja` | 都道府県+市区町村の簡易住所 | なし |

どの列タイプにも `null_rate`(0.0〜1.0)を追加でき、その確率でNULL(CSVでは空文字、SQLではクォートなしの`NULL`)を出力する。

生成される氏名・住所・電話番号・郵便番号はすべて固定リストからのランダムな組み合わせで、実在する個人・住所・番号とは一切関係がない。

## Dr.Sum / MotionBoard / Datalizer 等での利用について

これらの日本製BI/レポーティングツールはCSVの取り込み・出力の両方で文字コード(UTF-8/Shift-JIS)を指定できる。取り込み先の設定に合わせて `--encoding` を選ぶと文字化けを防げる。SQL(`--format sql`)は`INSERT INTO`文なので、Dr.SumのようにSQL実行機能を持つDB製品には直接流し込むことも可能。

## テスト

```bash
cargo test
```

バリデーション(min>maxの検出、不正な日付形式、null_rateの範囲チェックなど)やSQLエスケープ、CSVの行数・NULL表現を中心にユニットテストでカバーしている。

## パフォーマンスの実測値

10列×100万行のCSV生成(8コア環境)で計測:

- 値生成(rayonで並列化): 約0.55秒(逐次実行では約1.55秒)
- ディスクへの書き込み: 約1〜1.4秒(こちらがボトルネック。OS/ディスク側の制約でありCPUの並列化では改善できない)
