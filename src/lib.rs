// dummy_data_genの生成ロジック本体(エンジン部分)。CLI(main.rs)からも、
// dummygen_jp_gui(Tauri GUI・ブラウザ版サーバー)からも、このファイルの`pub`関数を
// ライブラリとして呼び出す形で使われる。ここには生成ロジックの入口が3段階ある。
//   1. `load_schema`: schema.yamlを読み込み、テーブル定義(Schema/SchemaFile)にする
//   2. `prepare_columns`/`prepare_tables`: 列定義を検証し、生成に使う実行時の形(PreparedColumn)に変換する
//   3. `generate_all_rows`/`write_csv_streaming`等: 実際に行データを作り、CSV/SQL/JSON/Excelとして書き出す
// 詳しい設計判断の背景は同じフォルダのCLAUDE.mdにまとめてある。
//
// ここからは、この後のコードを読むうえで最初につまずきやすいRustの基本構文を先にまとめておく。
//   - `use ○○::△△;`: 他のファイル(モジュール)や外部ライブラリ(クレート)にある機能を、
//     このファイルの中で名前だけで使えるようにする宣言。他の言語のimportに近い
//   - `struct`: 複数の値をひとまとめにした「型」を作る仕組み(他の言語のクラス/構造体に近い)
//   - `enum`: 「決まった選択肢のうちどれか1つ」を表す型(例: Format::Csv/Sql/Json/Xlsxのどれか)
//   - `impl 型名 { ... }`: その型に関数(メソッド)や、他のトレイト(後述)の実装をひも付ける場所
//   - `pub`: 他のファイルからも見える(公開されている)という印。無いとこのファイルの中だけで使える
//   - `#[derive(...)]`: そのstruct/enumに、決まったよくある機能を自動で追加してもらう目印
//     (例: Cloneなら複製できるようにする、Deserializeなら「YAML/JSONから読み込めるようにする」)
//   - `Option<T>`: 「値があるかもしれないし、無いかもしれない」ことを表す型。値があるときはSome(値)、
//     無いときはNoneになる(他の言語のnullに近いが、Rustでは必ずこの型を通して明示的に扱う)
//   - `Vec<T>`: 同じ型の値を可変長で並べたリスト(配列)
//   - `Result<T, E>`: 「成功したらOk(値)、失敗したらErr(エラー)」のどちらかを表す型。
//     関数の戻り値としてよく使われ、エラーが起きうる処理には基本的にこの型を使う
//   - `?`演算子: `Result`を返す式の直後に付けると、「Errだったらこの関数もそこで即座にErrを
//     返して終わる、Okだったら中身の値を取り出して続きを実行する」という省略記法になる
use chrono::Datelike;
use clap::ValueEnum;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

// 行番号ごとに独立したRNG(乱数生成器。Random Number Generatorの略)を作る関数。
// base_seedが同じなら常に同じ値になるため、並列実行(rayon)でどのスレッドがどの行を
// 処理しても結果が変わらない。wrapping_addは「足し算した結果が桁あふれ(オーバーフロー)
// してもエラーにせず、あふれた分を切り捨てて計算を続ける」足し算(row_numがどんな値でも
// 必ず計算が完了する)。SmallRng(暗号強度は無いが高速なPRNG=疑似乱数生成器)を使うのは、
// ダミーデータ生成に暗号学的な安全性は不要なため。
pub fn row_rng(base_seed: u64, row_num: u32) -> SmallRng {
    SmallRng::seed_from_u64(base_seed.wrapping_add(row_num as u64))
}

// 出力形式を表すenum(csv/sql/json/xlsxのどれか1つだけを取りうる)。
// #[derive(...)]は、このenumに次の機能を自動で追加している:
//   Clone, Copy: 値を複製できるようにする(Copyがあると、代入のたびに複製が自動で起きる)
//   ValueEnum: clapライブラリが「--format csv」のようなコマンドライン引数として
//     このenumを直接受け取れるようにする(#[value(name = "csv")]で対応する文字列を指定している)
//   PartialEq, Eq: `==`で比較できるようにする
//   Hash: HashMapやHashSetのキーとして使えるようにする
#[derive(Clone, Copy, ValueEnum, PartialEq, Eq, Hash)]
pub enum Format {
    #[value(name = "csv")]
    Csv,
    #[value(name = "sql")]
    Sql,
    #[value(name = "json")]
    Json,
    #[value(name = "xlsx")]
    Xlsx,
}

// Displayは「この値を人間が読める文字列にする方法」を定義するRustの仕組み(トレイトと呼ぶ)。
// これを実装しておくと、`println!("{}", format)`のような書き方でFormatの値を
// 文字列として表示できるようになる(mainの成功メッセージ表示で使われている)
impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Format::Csv => write!(f, "csv"),
            Format::Sql => write!(f, "sql"),
            Format::Json => write!(f, "json"),
            Format::Xlsx => write!(f, "xlsx"),
        }
    }
}

// schema.yaml の中身をそのまま受け止める箱(struct)。serdeというライブラリが自動で
// YAML→struct(この形)に変換してくれる。「1テーブル分の定義」を表す型で、単一テーブル
// 形式(schema.yamlのトップレベルにrow_count/columnsを直接書く形式)でも、複数テーブル
// 形式(tables:の各要素)でも、どちらも同じこの型として読み込む。tables:形式では
// 要素のキーが"name"なので、aliasでtable_nameとしても受け取れるようにしている。
// Serializeも付けているのは、GUI(dummygen_jp_gui)の「列設定をYAMLとして書き出す」機能
// (schema_file_to_yaml)のため。読み込み(Deserialize)は元々のload_schema用
#[derive(Deserialize, Serialize)]
pub struct Schema {
    pub row_count: u32,
    // SQL出力(--format sql)のときや、tables:形式でのテーブル名として使う。
    // #[serde(...)]はserdeへの細かい指示で、defaultは「YAMLに書かれていなければ
    // Noneのままにする」、alias = "name"は「"table_name"の代わりに"name"というキーで
    // 書かれていても同じ扱いにする」、skip_serializing_if(書き出すときのみ関係)は
    // 「値がNoneなら、YAMLに書き出すときこのキー自体を省略する」という意味
    #[serde(default, alias = "name", skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub columns: Vec<ColumnDef>,
}

// YAMLの最上位をいったん全部Optionとして受け止める箱。単一テーブル形式(row_count/columns
// が直接トップレベルにある)と複数テーブル形式(tables:のリスト)のどちらで書かれているかを
// ここではまだ判定しない(判定はnormalize_schema_fileで行う)。全フィールドをOptionにして
// おくことで、「どちらの形式でも、書かれていないキーがあってもエラーにせず読み込める」箱になる。
// serdeのuntagged enumを使わないのは、untaggedだと「どのバリアントにも一致しません」という
// 分かりにくいエラーになり、既存の日本語エラーメッセージの分かりやすさが損なわれるため。
#[derive(Deserialize)]
struct RawSchemaFile {
    #[serde(default)]
    row_count: Option<u32>,
    #[serde(default)]
    table_name: Option<String>,
    #[serde(default)]
    columns: Option<Vec<ColumnDef>>,
    #[serde(default)]
    tables: Option<Vec<Schema>>,
}

// normalize_schema_fileの結果。multi_tableは出力パスの決め方の分岐に使う
// (tables:形式で明示的に書かれていたかどうか。単一テーブル形式ならfalse)
pub struct SchemaFile {
    pub tables: Vec<Schema>,
    pub multi_table: bool,
}

// 複数テーブル形式で「テーブル名が空(または空白だけ)になっていないか」「同じ名前の
// テーブルが2つ以上無いか」を検証する。schema.yaml読み込み時(normalize_schema_file)と、
// dummygen_jp_guiから直接呼ばれる複数テーブル生成・プレビュー(prepare_tables。
// normalize_schema_fileを経由しないため、ここで検証しないとテーブル名の重複が
// そのまま素通りしてしまう)の両方から使う共通チェック。
fn validate_multi_table_names(tables: &[Schema]) -> Result<(), Box<dyn std::error::Error>> {
    // iter()で一覧を1件ずつ取り出し、enumerate()で「0番目、1番目、…」という
    // 連番(i)も一緒に取り出す。for (i, table) in ... はその2つをそれぞれ
    // 変数i・tableとして受け取りながら繰り返す構文
    for (i, table) in tables.iter().enumerate() {
        // as_deref()はOption<String>をOption<&str>に変換するメソッド、
        // is_none_or(...)は「Noneなら true、Someなら中身を関数(ここではstr::is_empty=
        // 「空文字かどうか」)に渡した結果を返す」という判定。つまりここは
        // 「テーブル名が指定されていない、または空文字である」ことを調べている
        // is_none_or(...)の中身をstr::is_emptyから「trim後が空文字か」に変えることで、
        // 未指定・空文字だけでなく空白だけの名前(例: "   ")も同じくエラーにする
        if table.table_name.as_deref().is_none_or(|s| s.trim().is_empty()) {
            return Err(format!("tables[{}]: テーブル名(name)を指定してください", i).into());
        }
    }
    // 二重のfor文で「全てのテーブルの組み合わせ」を1つずつ比較し、同じ名前が
    // 無いか確認する。外側のiが0,1,2...と進み、内側のjは常に「iより後ろ」の
    // 範囲((i + 1)..tables.len())だけを見るので、同じペアを2回比較しなくて済む
    for i in 0..tables.len() {
        for j in (i + 1)..tables.len() {
            if tables[i].table_name == tables[j].table_name {
                return Err(format!(
                    "テーブル名 \"{}\" が重複しています",
                    tables[i].table_name.as_deref().unwrap_or("")
                )
                .into());
            }
        }
    }
    Ok(())
}

// RawSchemaFile(YAMLをそのまま受け止めた、全部Option状態の箱)を見て、単一テーブル形式か
// 複数テーブル形式かを判定し、どちらの場合も同じSchemaFileの形に揃える(正規化する)関数。
fn normalize_schema_file(raw: RawSchemaFile) -> Result<SchemaFile, Box<dyn std::error::Error>> {
    // is_some()は「Optionの中身がSome(値がある)かどうか」をtrue/falseで返すメソッド。
    // ||は「または」を表す論理演算子で、3つのうち1つでもtrueなら全体がtrueになる
    let has_single_table_fields =
        raw.row_count.is_some() || raw.columns.is_some() || raw.table_name.is_some();

    // "if let Some(tables) = raw.tables"は、「raw.tablesの中身がSome(つまりtables:が
    // 書かれていた)なら、その中身を変数tablesとして取り出して{}の中を実行する」という構文
    // (Noneだった場合は{}の中を素通りして、この下の単一テーブル用の処理に進む)
    if let Some(tables) = raw.tables {
        if has_single_table_fields {
            // Err(...)で関数の戻り値をエラーとして返す。文字列(&str)は.into()で
            // 自動的にBox<dyn std::error::Error>(エラーを表す型)に変換される
            return Err(
                "tables: と row_count:/columns:/table_name: は同時に指定できません。複数テーブルを作る場合は各テーブルの定義を tables: の中に書いてください".into(),
            );
        }
        if tables.is_empty() {
            return Err("tables には少なくとも1つ以上のテーブルを定義してください".into());
        }
        validate_multi_table_names(&tables)?;
        // Ok(...)で関数の戻り値を「成功」として返す(中身はここまでで検証済みのtables)
        return Ok(SchemaFile { tables, multi_table: true });
    }

    // ここに来るのは、tables:が書かれていなかった(=単一テーブル形式のはず)場合。
    // ok_or(...)は「Optionの中身がSomeならその値を、Noneなら指定したエラーメッセージで
    // Errにする」変換。直後の?は「Errならこの関数もそこでErrを返して終わる」という意味なので、
    // この2行は「columns/row_countが無ければエラーメッセージ付きで即座に終了する」処理になる
    let columns = raw
        .columns
        .ok_or("columns には少なくとも1つ以上の列を定義してください")?;
    let row_count = raw.row_count.ok_or("row_count を指定してください")?;

    Ok(SchemaFile {
        tables: vec![Schema { row_count, table_name: raw.table_name, columns }],
        multi_table: false,
    })
}

// 列1個分の設定を表す型。全ての列タイプ(sequence/name_ja/integer/...)に共通する
// name/null_rate/uniqueと、型ごとに違う追加設定(column_type)を持つ
#[derive(Deserialize, Serialize)]
pub struct ColumnDef {
    pub name: String,
    // 0.0〜1.0の確率でNULL(空)を混ぜる。省略時はNULLを混ぜない(0.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub null_rate: Option<f64>,
    // trueにすると、この列の値が行間で重複しないようにする。省略時はfalse
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
    // #[serde(flatten)]を付けると、column_type(ColumnType型、下で定義)が持つ
    // フィールド(typeやmin/max等)を、入れ子にせずnameと同じ階層に展開して読み書きできる。
    // これにより、YAML上は "name: id" と "type: integer" を同じ階層に並べて書ける
    #[serde(flatten)]
    pub column_type: ColumnType,
}

// 列のタイプ(sequence/name_ja/integer/...)を表すenum。Rustのenumは「バリアントごとに
// 違うデータを持てる」のが特徴で、例えばIntegerはmin/maxを持つがBooleanは何も持たない、
// というように列タイプごとに必要な追加情報だけを持たせられる。
// #[serde(tag = "type", rename_all = "snake_case")]は、YAML上の"type:"というキーの値
// (例: "integer")を見てどのバリアントかを判定し(内部タグ付きenumと呼ぶ)、min/maxなど
// 残りのフィールドをそのバリアントの中身として読み取ってくれる、というserdeへの指示。
// rename_all = "snake_case"は、バリアント名(Integer)をYAML上では小文字+アンダースコア
// (integer)に変換する、という意味(Rust側は大文字始まりの命名規則を使うため)
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ColumnType {
    Sequence,
    NameJa {
        // trueのとき姓と名の間にスペースを入れる(例: "山田 太郎")。
        // 省略時はfalse(既存のschema.yamlとの後方互換のため、今まで通り"山田太郎")
        #[serde(default)]
        with_space: bool,
    },
    // 姓・名をそれぞれ単独で生成する列タイプ(name_jaはフルネーム一体型なので、
    // 姓だけ・名だけが欲しい場合に使う。katakana_nameのような列間の対応付けはしない)
    LastNameJa,
    FirstNameJa,
    // name_jaのローマ字(ヘボン式)版。直前のname_ja列を参照する挙動はkatakana_nameと同じ
    RomajiName,
    // katakana_nameとは異なり、last_name_ja/first_name_jaと同様に列間の対応付けをしない
    // 独立ランダムなフリガナ(姓・名それぞれ単独)
    KatakanaLastName,
    KatakanaFirstName,
    Email {
        // 省略時は"example.com"(既存のschema.yamlとの後方互換のため)
        #[serde(default = "default_email_domain")]
        domain: String,
    },
    Integer {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
        #[serde(default = "default_decimals")]
        decimals: u32,
    },
    Boolean,
    // "男性"または"女性"を50%ずつのランダムで返す
    Gender,
    // "A型"/"O型"/"B型"/"AB型"を日本人の血液型分布に近い比率(4:3:2:1目安)で重み付けして返す
    BloodType,
    // 正規表現に似た簡易パターンから値を生成する(例: "[A-Z]{3}-[0-9]{4}" → "ABC-1234")。
    // 文法の詳細はcompile_patternのコメントを参照
    Pattern {
        pattern: String,
    },
    Date {
        start: String,
        end: String,
        #[serde(default = "default_date_format")]
        format: DateFormat,
    },
    // min_age〜max_age歳になる生年月日を、今日の日付を基準に逆算して生成する
    BirthDate {
        min_age: u32,
        max_age: u32,
        #[serde(default = "default_date_format")]
        format: DateFormat,
    },
    PostalCode,
    PhoneJa,
    // 携帯電話番号(phone_ja)とは別に、市外局番付きの固定電話番号を生成する
    PhoneJaLandline,
    AddressJa,
    CompanyNameJa,
    Uuid,
    PrefectureJa,
    CityJa,
    KatakanaName {
        // name_jaのwith_spaceと同じ意味・既定値(省略時false=既存のschema.yamlと同じ姓名連結)。
        // trueのとき姓の読みと名の読みの間に半角スペースを入れる(例: "ヤマダ タロウ")
        #[serde(default)]
        with_space: bool,
    },
    // katakana_nameの半角カタカナ版。直前のname_ja列を参照する挙動・with_spaceの意味は同じ
    KatakanaNameHankaku {
        #[serde(default)]
        with_space: bool,
    },
    DepartmentJa,
    JobTitleJa,
    // IPv4のみ対応(IPv6は現状スコープ外)
    IpAddress,
    // 本物の署名検証はできない「それっぽい形」のダミー値(ヘッダー部分は固定文字列)
    Jwt,
    ApiKey,
    // ローマ字氏名の名(FIRST_NAMES_ROMAJI)を小文字にしたもの+3桁の数字(例: "taro123")
    Username,
    // 英大文字/小文字/数字/記号を混ぜた12文字(本物のパスワードではなくダミー値)
    Password,
    // プレースホルダー画像サービス(placehold.jp)のURL文字列。実際に画像を取得したりはしない
    ProfileImageUrl,
    // 16桁、Luhnアルゴリズムで検査数字(末尾1桁)を計算する
    CreditCardNumber,
    // "MM/YY"形式(今日から1〜5年後のランダムな年月)
    CreditCardExpiry,
    // 日本の普通預金口座番号を想定した7桁のゼロ埋め数字
    BankAccountNumber,
    // "SKU-"+英大文字/数字8文字
    ProductSku,
    // 12桁、個人番号法で定められた重み付け剰余演算で検査数字(末尾1桁)を計算する
    MyNumber,
    Enum {
        choices: Vec<String>,
        // choicesと同じ個数だけ指定すると、出現確率に偏りをつけられる(例: [7.0, 2.0, 1.0])。
        // 値そのものの大小に意味はなく、他の要素との比率だけが結果を左右する。
        // 省略時(None)は今まで通り均等な確率(既存のschema.yamlとの後方互換のため)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weights: Option<Vec<f64>>,
    },
    // どの行でも常に同じ文字列を返す列
    Fixed {
        value: String,
    },
    // "テーブル名.列名" 形式(最初の"."で分割)。複数テーブル形式(tables:)でのみ使える
    ForeignKey {
        references: String,
    },
}

fn default_decimals() -> u32 {
    2
}

fn default_email_domain() -> String {
    "example.com".to_string()
}

// date/birth_date列の日付表示形式。Iso8601とYmdは日付のみの表記では見た目が同じ(YYYY-MM-DD)
// だが、利用者が明示的に選べるよう別の選択肢として用意してある
#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    Ymd,
    Iso8601,
    Slash,
    Wareki,
}

fn default_date_format() -> DateFormat {
    DateFormat::Ymd
}

fn format_date(date: chrono::NaiveDate, format: DateFormat) -> String {
    match format {
        DateFormat::Ymd | DateFormat::Iso8601 => date.format("%Y-%m-%d").to_string(),
        DateFormat::Slash => date.format("%Y/%m/%d").to_string(),
        DateFormat::Wareki => format_wareki(date),
    }
}

// 元号の開始日(グレゴリオ暦)。新しい元号から順に並べておき、date以上で最も新しい
// 開始日を持つ元号を採用する。明治より前の日付は明治として扱う(ダミーデータの
// 日付範囲は通常これで十分なため、birth_dateの365日/年近似と同様の意図的な単純化)
const ERAS: &[(&str, i32, u32, u32)] = &[
    ("令和", 2019, 5, 1),
    ("平成", 1989, 1, 8),
    ("昭和", 1926, 12, 25),
    ("大正", 1912, 7, 30),
    ("明治", 1868, 1, 25),
];

fn format_wareki(date: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    let (era_name, era_start_year) = ERAS
        .iter()
        .find_map(|(name, y, m, d)| {
            let start = chrono::NaiveDate::from_ymd_opt(*y, *m, *d).expect("ERAS内の日付は常に有効");
            (date >= start).then_some((*name, *y))
        })
        .unwrap_or((ERAS.last().expect("ERASは空でない").0, ERAS.last().expect("ERASは空でない").1));
    let year_in_era = date.year() - era_start_year + 1;
    let year_label = if year_in_era == 1 { "元年".to_string() } else { format!("{year_in_era}年") };
    format!("{}{}{}月{}日", era_name, year_label, date.month(), date.day())
}

pub fn load_schema(path: &str) -> Result<SchemaFile, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("スキーマファイル({})の読み込みに失敗しました: {}", path, e))?;
    let raw: RawSchemaFile = serde_yaml::from_str(&text)?;
    normalize_schema_file(raw)
}

// tables:形式でYAMLに書き出すときだけ使う、tablesキー1つだけの小さな箱
#[derive(Serialize)]
struct TablesOnly<'a> {
    tables: &'a [Schema],
}

// SchemaFileをschema.yamlと同じ見た目のYAML文字列にする(load_schemaの逆方向)。
// GUI(dummygen_jp_gui)の「列設定をYAMLとして保存」機能で使う。
// テーブルが1個だけなら、そのテーブルをトップレベルに直接書く単一テーブル形式
// (手書きのschema.yamlと同じ見た目)にする。2個以上ならtables:形式にする
pub fn schema_file_to_yaml(file: &SchemaFile) -> Result<String, Box<dyn std::error::Error>> {
    if file.tables.len() == 1 {
        Ok(serde_yaml::to_string(&file.tables[0])?)
    } else {
        Ok(serde_yaml::to_string(&TablesOnly { tables: &file.tables })?)
    }
}

// YAMLから読んだそのままの定義(ColumnType)を、実際に値を作るときに必要な形に変換したもの。
// 日付は文字列のままだと1行作るたびに毎回パースし直すことになり10万行規模で無駄なので、
// ここで先に1回だけ計算しておく。min > max のような矛盾も、生成が始まる前にここで弾く。
pub struct PreparedColumn {
    name: String,
    kind: PreparedColumnType,
    null_rate: f64,
    unique: bool,
    // uniqueなら、あらかじめ計算しておいた「行数分の重複しない値」がここに入る
    // (resolve_unique_pools が base_seed が決まった後に埋める)。
    // Some(pool)のとき、generate_cellはこの中から順番に値を取り出すだけになる
    unique_pool: Option<Vec<String>>,
}

pub enum PreparedColumnType {
    Sequence,
    NameJa { with_space: bool },
    LastNameJa,
    FirstNameJa,
    RomajiName,
    KatakanaLastName,
    KatakanaFirstName,
    Email { domain: String },
    Integer { min: i64, max: i64 },
    Float { min: f64, max: f64, decimals: u32 },
    Boolean,
    Gender,
    BloodType,
    Pattern { pieces: Vec<PatternPiece> },
    Date { start_days: i32, span_days: i64, format: DateFormat },
    BirthDate { start_days: i32, span_days: i64, format: DateFormat },
    PostalCode,
    PhoneJa,
    PhoneJaLandline,
    AddressJa,
    CompanyNameJa,
    Uuid,
    PrefectureJa,
    CityJa,
    KatakanaName { with_space: bool },
    KatakanaNameHankaku { with_space: bool },
    DepartmentJa,
    JobTitleJa,
    IpAddress,
    Jwt,
    ApiKey,
    Username,
    Password,
    ProfileImageUrl,
    CreditCardNumber,
    CreditCardExpiry,
    BankAccountNumber,
    ProductSku,
    MyNumber,
    Enum { choices: Vec<String>, weights: Option<Vec<f64>> },
    Fixed { value: String },
    ForeignKey {
        ref_table: String,
        ref_column: String,
        // 参照先の列タイプから決まる「値の見え方」。SQLのクォート要否やJSON/Excelの型判定に使う。
        // 既定はText。resolve_fk_reprsが親テーブルの列を見て確定させる
        repr: FkRepr,
        // 親テーブルを生成した後にfill_foreign_key_poolsが埋める。
        // 生成時はこの中から一様ランダムに1つ選ぶだけになる(unique_poolと同じ二段構えの設計)。
        // Arcなのは、同じ親列を複数の子列が参照しても実体を1つで共有するため
        pool: Option<Arc<Vec<String>>>,
    },
}

// patternの1個分(文字または文字クラス)と、その繰り返し回数の範囲。
// 例えば "[A-Z]{3}" は「文字クラス[A-Z]を3回繰り返す」なのでPatternPiece{
// chars: ['A'..'Z'の26文字], min_repeat: 3, max_repeat: 3 }になる。
// 文字クラスは(否定"^"も含めて)全て「実際に選べる文字の一覧」に展開してから持つので、
// 生成時はcharsからrng.gen_range(0..chars.len())で1文字選ぶだけでよい
pub struct PatternPiece {
    chars: Vec<char>,
    min_repeat: u32,
    max_repeat: u32,
}

// {n,}や*のような上限の無い量指定子の、実務上十分な繰り返し回数の上限
// (無制限にすると1文字だけの列が数十文字になり得て、値として不自然になるため)
const PATTERN_MAX_UNBOUNDED_REPEAT: u32 = 12;

// patternで使える否定文字クラス"[^...]"の元になる文字の範囲(印字可能なASCII、空白を除く)。
// 日本語など全角文字はpatternでは非対応(半角の記号・英数字を組み合わせる用途を想定しているため)
fn pattern_default_charset() -> Vec<char> {
    (0x21u8..=0x7eu8).map(|b| b as char).collect()
}

/// 正規表現に似た簡易パターン文字列を、生成時にすぐ使える`Vec<PatternPiece>`に変換する。
/// 対応する構文:
/// - 通常の文字はそのまま1文字のリテラルになる(例: "ABC" → A,B,C)
/// - `\` の次の1文字はリテラル文字として扱う(例: `\-`は記号の"-"そのもの、`\\`は"\")
/// - `[...]`は文字クラス。`a-z`のような範囲、`0-9a-fA-F`のような複数範囲の組み合わせ、
///   先頭の`^`で「それ以外の印字可能なASCII文字」を意味する否定に対応する
/// - 直前の1文字またはクラスに続けて量指定子を書ける: `?`(0か1回)/`*`(0〜12回)/
///   `+`(1〜12回)/`{n}`(ちょうどn回)/`{n,}`(n〜12回)/`{n,m}`(n〜m回)
/// - グループ化`(...)`や選択`|`には対応しない(必要になれば別途拡張する)
// 直前の1文字/文字クラス(pending)を、量指定子が無いまま次の要素が来た場合に
// 「ちょうど1回」のPatternPieceとして確定させる(pendingが無ければ何もしない)
fn flush_default(pending: &mut Option<Vec<char>>, pieces: &mut Vec<PatternPiece>) {
    if let Some(chars) = pending.take() {
        pieces.push(PatternPiece { chars, min_repeat: 1, max_repeat: 1 });
    }
}

// 量指定子(?,*,+,{...})を読んだときに、直前のpendingをその繰り返し回数で確定させる。
// pendingが無い(量指定子の直前に文字が無い)場合はエラーにする
fn flush_quantified(
    pending: &mut Option<Vec<char>>,
    pieces: &mut Vec<PatternPiece>,
    min: u32,
    max: u32,
) -> Result<(), String> {
    let chars = pending.take().ok_or_else(|| "量指定子(?,*,+,{...})の直前に文字が無い".to_string())?;
    pieces.push(PatternPiece { chars, min_repeat: min, max_repeat: max });
    Ok(())
}

// pattern文字列を、先頭から1文字(または[...]のかたまり)ずつ順番に読み進めながら
// PatternPieceの一覧に変換していく、自作の小さな構文解析器(パーサ)。
// 変数iが「今どこまで読んだか」を表すカーソル(読み取り位置)で、while文の中でiを
// 少しずつ進めながら、chars[i]が何の文字かによって処理を振り分ける(match文)。
// chars.get(i)は「i番目の文字を取り出すが、範囲外ならNoneを返す(配列外アクセスで
// 落ちない)安全な取り出し方」で、文字列の終わりを超えて読もうとしていないかの
// チェックを兼ねている
fn compile_pattern(pattern: &str) -> Result<Vec<PatternPiece>, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut pieces = Vec::new();
    let mut i = 0;

    // 直前に確定した「1文字ぶんの候補一覧」を、量指定子が来るまで一時的に持っておく
    let mut pending: Option<Vec<char>> = None;

    while i < chars.len() {
        match chars[i] {
            // "\"(バックスラッシュ)は次の1文字をそのままリテラル文字として扱うためのエスケープ。
            // 直前のpendingを先に確定させ(flush_default)、エスケープした文字を新しいpendingにする
            '\\' => {
                i += 1;
                let escaped = *chars.get(i).ok_or_else(|| "パターンの末尾が\\で終わっている".to_string())?;
                flush_default(&mut pending, &mut pieces);
                pending = Some(vec![escaped]);
                i += 1;
            }
            // "["は文字クラス(例: "[A-Z]")の始まり。"]"が出てくるまで、1文字ずつ、または
            // "a-z"のような範囲指定を読み取り、実際に選べる文字を全部BTreeSet(重複を持たず、
            // 順序も保たれる集合)に集めていく
            '[' => {
                i += 1;
                flush_default(&mut pending, &mut pieces);
                // 先頭が"^"なら「それ以外の文字」を意味する否定文字クラスになる
                let negate = chars.get(i) == Some(&'^');
                if negate {
                    i += 1;
                }
                let mut set = std::collections::BTreeSet::new();
                while chars.get(i) != Some(&']') {
                    let start = *chars.get(i).ok_or_else(|| "文字クラス[...]が]で閉じられていない".to_string())?;
                    // "a-z"のような「1文字、ハイフン、1文字」の並びなら範囲指定とみなす。
                    // is_some_and(...)は「Optionの中身がSomeで、かつ指定した条件も満たすか」を判定するメソッド
                    if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|c| *c != ']') {
                        let end = chars[i + 2];
                        if start > end {
                            return Err(format!("文字クラスの範囲が逆順になっている: {start}-{end}"));
                        }
                        // start..=endで「startからendまでの文字」を1つずつ取り出し、集合に追加する
                        for c in start..=end {
                            set.insert(c);
                        }
                        i += 3;
                    } else {
                        set.insert(start);
                        i += 1;
                    }
                }
                i += 1; // ']'を読み飛ばす
                // 否定文字クラスなら「印字可能なASCII全体から、集めた文字を除いたもの」、
                // そうでなければ「集めた文字そのもの」が実際の候補になる
                let resolved: Vec<char> = if negate {
                    pattern_default_charset().into_iter().filter(|c| !set.contains(c)).collect()
                } else {
                    set.into_iter().collect()
                };
                if resolved.is_empty() {
                    return Err("文字クラス[...]の候補が0文字になった".to_string());
                }
                pending = Some(resolved);
            }
            '?' => {
                flush_quantified(&mut pending, &mut pieces, 0, 1)?;
                i += 1;
            }
            '*' => {
                flush_quantified(&mut pending, &mut pieces, 0, PATTERN_MAX_UNBOUNDED_REPEAT)?;
                i += 1;
            }
            '+' => {
                flush_quantified(&mut pending, &mut pieces, 1, PATTERN_MAX_UNBOUNDED_REPEAT)?;
                i += 1;
            }
            '{' => {
                let close = chars[i..].iter().position(|c| *c == '}').ok_or_else(|| "{が}で閉じられていない".to_string())?;
                let body: String = chars[i + 1..i + close].iter().collect();
                let (min, max) = match body.split_once(',') {
                    Some((min, "")) => {
                        let min = min.parse::<u32>().map_err(|_| format!("不正な繰り返し回数指定: {{{body}}}"))?;
                        (min, PATTERN_MAX_UNBOUNDED_REPEAT.max(min))
                    }
                    Some((min, max)) => {
                        let min = min.parse::<u32>().map_err(|_| format!("不正な繰り返し回数指定: {{{body}}}"))?;
                        let max = max.parse::<u32>().map_err(|_| format!("不正な繰り返し回数指定: {{{body}}}"))?;
                        (min, max)
                    }
                    None => {
                        let n = body.parse::<u32>().map_err(|_| format!("不正な繰り返し回数指定: {{{body}}}"))?;
                        (n, n)
                    }
                };
                if min > max {
                    return Err(format!("繰り返し回数の範囲が逆順になっている: {{{body}}}"));
                }
                flush_quantified(&mut pending, &mut pieces, min, max)?;
                i += close + 1;
            }
            ']' | '}' => return Err(format!("対応する開き括弧の無い'{}'がある", chars[i])),
            // グループ化"(...)"と選択"a|b"は非対応の構文(README/CLAUDE.md記載の通り)。
            // 対応表に無い記号として黙って1文字ずつのリテラル扱いにしてしまうと、
            // 例えば"(abc)"や"a|b"がそのまま固定文字列として生成されてしまい、
            // ユーザーが意図した挙動(繰り返しのグループ化・二択)が全く効かないのに
            // 気づきにくい。他の非対応構文と同様、はっきりエラーにする
            '(' | ')' | '|' => {
                return Err(format!(
                    "'{}'はこのpatternでは使えません(グループ化\"(...)\"や選択\"a|b\"は非対応です)",
                    chars[i]
                ));
            }
            c => {
                flush_default(&mut pending, &mut pieces);
                pending = Some(vec![c]);
                i += 1;
            }
        }
    }
    flush_default(&mut pending, &mut pieces);

    if pieces.is_empty() {
        return Err("patternが空文字列になっている".to_string());
    }
    Ok(pieces)
}

// 外部キーの値を、出力形式ごとにどう扱うか(参照先の列タイプから決まる)
#[derive(Clone, Copy, PartialEq)]
pub enum FkRepr {
    Integer,
    Float,
    Boolean,
    Text,
}

fn fk_repr_of(kind: &PreparedColumnType) -> FkRepr {
    match kind {
        PreparedColumnType::Sequence | PreparedColumnType::Integer { .. } => FkRepr::Integer,
        PreparedColumnType::Float { .. } => FkRepr::Float,
        PreparedColumnType::Boolean => FkRepr::Boolean,
        // 多段参照(親自身もforeign_key列)の場合は、親のreprをそのまま引き継ぐ
        PreparedColumnType::ForeignKey { repr, .. } => *repr,
        _ => FkRepr::Text,
    }
}

// "users.id" のような参照先指定を最初の"."で分割する(列名側には"."を含められる)
fn parse_reference(
    references: &str,
    column_name: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    match references.split_once('.') {
        Some((table, column)) if !table.is_empty() && !column.is_empty() => {
            Ok((table.to_string(), column.to_string()))
        }
        _ => Err(format!(
            "列 \"{}\": references は \"テーブル名.列名\" の形式で指定してください(例: users.id)",
            column_name
        )
        .into()),
    }
}

// schema.yamlから読み込んだ列定義(Schema、文字列や生の数値がそのまま入っている)を検証し、
// 実際の生成処理で使う実行時の形(PreparedColumn)に変換する関数。処理の流れ:
//   1. 列が1つも無ければエラー
//   2. 列名の重複が無いかを確認する(二重ループで全ての組み合わせを比較)
//   3. 各列について、型ごとに必要なバリデーション(min>maxでないか、日付形式が正しいか等)を行い、
//      問題なければColumnType(YAML由来の型)をPreparedColumnType(生成処理で使う型)に変換する
//   4. 最後に、null_rate/uniqueの指定が正しいかもチェックする(この関数の後半に続く)
pub fn prepare_columns(schema: &Schema) -> Result<Vec<PreparedColumn>, Box<dyn std::error::Error>> {
    if schema.columns.is_empty() {
        return Err("columns には少なくとも1つ以上の列を定義してください".into());
    }
    if schema.row_count == 0 {
        return Err("row_count には1以上を指定してください".into());
    }

    // 全ての列の組み合わせを1つずつ比較し、同じ名前の列が無いかを確認する
    // (normalize_schema_fileのテーブル名重複チェックと同じ「二重ループで総当たり」の考え方)
    for i in 0..schema.columns.len() {
        // trim()で前後の空白を取り除いた結果が空文字なら、空文字自体はもちろん
        // 空白だけの列名(例: "   ")も列名としては使えないため弾く
        if schema.columns[i].name.trim().is_empty() {
            return Err("列名は空文字・空白だけにはできません".into());
        }
        for j in (i + 1)..schema.columns.len() {
            if schema.columns[i].name == schema.columns[j].name {
                return Err(format!(
                    "列名 \"{}\" が重複しています。列名は一意にしてください",
                    schema.columns[i].name
                )
                .into());
            }
        }
    }

    // schema.columns(YAMLから読み込んだ列の一覧)を1つずつ処理し、それぞれ
    // PreparedColumn(実行時用の形)に変換した新しい一覧を作る。
    // iter()で1件ずつ取り出し、.map(|c| { ... })で「1列分の変換処理」を各列に適用し、
    // 最後の.collect()で全部まとめる(下の方にある)。map内の処理でErrを返すと、
    // .collect::<Result<...>>()がその時点で処理を打ち切り、全体としてErrを返す
    // (詳しくはこの関数の末尾、.collect()の行を参照)
    schema
        .columns
        .iter()
        .map(|c| {
            // c.column_type(YAMLに書かれた列タイプ)を見て、対応するPreparedColumnTypeに
            // 変換する。ほとんどの型は値をコピーするだけだが、min/maxのように「値として
            // おかしくないか」の確認が必要な型は、ここでチェックしてから変換している
            let kind = match &c.column_type {
                ColumnType::Sequence => PreparedColumnType::Sequence,
                ColumnType::NameJa { with_space } => PreparedColumnType::NameJa { with_space: *with_space },
                ColumnType::LastNameJa => PreparedColumnType::LastNameJa,
                ColumnType::FirstNameJa => PreparedColumnType::FirstNameJa,
                ColumnType::RomajiName => PreparedColumnType::RomajiName,
                ColumnType::KatakanaLastName => PreparedColumnType::KatakanaLastName,
                ColumnType::KatakanaFirstName => PreparedColumnType::KatakanaFirstName,
                ColumnType::Email { domain } => PreparedColumnType::Email { domain: domain.clone() },
                ColumnType::Integer { min, max } => {
                    if min > max {
                        return Err(format!(
                            "列 \"{}\": min({})がmax({})より大きくなっています",
                            c.name, min, max
                        )
                        .into());
                    }
                    PreparedColumnType::Integer { min: *min, max: *max }
                }
                ColumnType::Float { min, max, decimals } => {
                    if min > max {
                        return Err(format!(
                            "列 \"{}\": min({})がmax({})より大きくなっています",
                            c.name, min, max
                        )
                        .into());
                    }
                    PreparedColumnType::Float { min: *min, max: *max, decimals: *decimals }
                }
                ColumnType::Boolean => PreparedColumnType::Boolean,
                ColumnType::Gender => PreparedColumnType::Gender,
                ColumnType::BloodType => PreparedColumnType::BloodType,
                ColumnType::Pattern { pattern } => {
                    let pieces = compile_pattern(pattern).map_err(|e| format!("列 \"{}\": {}", c.name, e))?;
                    PreparedColumnType::Pattern { pieces }
                }
                ColumnType::Date { start, end, format } => {
                    let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
                        .map_err(|e| format!("列 \"{}\": start の日付形式が不正です({})", c.name, e))?;
                    let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
                        .map_err(|e| format!("列 \"{}\": end の日付形式が不正です({})", c.name, e))?;
                    let span_days = (end_date - start_date).num_days();
                    if span_days < 0 {
                        return Err(format!("列 \"{}\": start が end より後の日付です", c.name).into());
                    }
                    PreparedColumnType::Date {
                        start_days: start_date.num_days_from_ce(),
                        span_days,
                        format: *format,
                    }
                }
                ColumnType::BirthDate { min_age, max_age, format } => {
                    if min_age > max_age {
                        return Err(format!(
                            "列 \"{}\": min_age({})がmax_age({})より大きくなっています",
                            c.name, min_age, max_age
                        )
                        .into());
                    }
                    // 「今日」基準で年齢範囲から日付範囲を逆算する。365日/年の近似で計算しており
                    // (閏年や2/29生まれの扱いを気にする必要が無いようにするための意図的な単純化)、
                    // ダミーデータの年齢範囲を日単位で厳密に一致させる要件ではないため許容する
                    let today = chrono::Local::now().date_naive();
                    let earliest = today - chrono::Duration::days(365 * (*max_age as i64 + 1) + 1);
                    let latest = today - chrono::Duration::days(365 * *min_age as i64);
                    let span_days = (latest - earliest).num_days().max(0);
                    PreparedColumnType::BirthDate {
                        start_days: earliest.num_days_from_ce(),
                        span_days,
                        format: *format,
                    }
                }
                ColumnType::PostalCode => PreparedColumnType::PostalCode,
                ColumnType::PhoneJa => PreparedColumnType::PhoneJa,
                ColumnType::PhoneJaLandline => PreparedColumnType::PhoneJaLandline,
                ColumnType::AddressJa => PreparedColumnType::AddressJa,
                ColumnType::CompanyNameJa => PreparedColumnType::CompanyNameJa,
                ColumnType::Uuid => PreparedColumnType::Uuid,
                ColumnType::PrefectureJa => PreparedColumnType::PrefectureJa,
                ColumnType::CityJa => PreparedColumnType::CityJa,
                ColumnType::KatakanaName { with_space } => {
                    PreparedColumnType::KatakanaName { with_space: *with_space }
                }
                ColumnType::KatakanaNameHankaku { with_space } => {
                    PreparedColumnType::KatakanaNameHankaku { with_space: *with_space }
                }
                ColumnType::DepartmentJa => PreparedColumnType::DepartmentJa,
                ColumnType::JobTitleJa => PreparedColumnType::JobTitleJa,
                ColumnType::IpAddress => PreparedColumnType::IpAddress,
                ColumnType::Jwt => PreparedColumnType::Jwt,
                ColumnType::ApiKey => PreparedColumnType::ApiKey,
                ColumnType::Username => PreparedColumnType::Username,
                ColumnType::Password => PreparedColumnType::Password,
                ColumnType::ProfileImageUrl => PreparedColumnType::ProfileImageUrl,
                ColumnType::CreditCardNumber => PreparedColumnType::CreditCardNumber,
                ColumnType::CreditCardExpiry => PreparedColumnType::CreditCardExpiry,
                ColumnType::BankAccountNumber => PreparedColumnType::BankAccountNumber,
                ColumnType::ProductSku => PreparedColumnType::ProductSku,
                ColumnType::MyNumber => PreparedColumnType::MyNumber,
                ColumnType::Enum { choices, weights } => {
                    if choices.is_empty() {
                        return Err(format!("列 \"{}\": choices には1つ以上の選択肢が必要です", c.name).into());
                    }
                    // if let Some(w) = weights は「weightsが指定されていれば、その中身をwとして
                    // 取り出して{}を実行する」という構文(weightsは省略可能なOption型のため)
                    if let Some(w) = weights {
                        if w.len() != choices.len() {
                            return Err(format!(
                                "列 \"{}\": weights の個数({})は choices の個数({})と同じにしてください",
                                c.name,
                                w.len(),
                                choices.len()
                            )
                            .into());
                        }
                        // any(...)は「一覧の中に条件を満たす要素が1つでもあるか」を調べるメソッド。
                        // &vは「一覧の中身を1つずつ指す参照」で、v < 0.0で「負の数かどうか」を見る
                        if w.iter().any(|&v| v < 0.0) {
                            return Err(format!("列 \"{}\": weights に負の数は指定できません", c.name).into());
                        }
                        // sum()は一覧の値を全部足し算するメソッド。::<f64>は「f64(小数)として
                        // 合計する」という型の指定(書かないと合計の型が決められない場合がある)
                        if w.iter().sum::<f64>() <= 0.0 {
                            return Err(format!("列 \"{}\": weights の合計は0より大きくしてください", c.name).into());
                        }
                    }
                    PreparedColumnType::Enum { choices: choices.clone(), weights: weights.clone() }
                }
                ColumnType::Fixed { value } => PreparedColumnType::Fixed { value: value.clone() },
                ColumnType::ForeignKey { references } => {
                    let (ref_table, ref_column) = parse_reference(references, &c.name)?;
                    PreparedColumnType::ForeignKey { ref_table, ref_column, repr: FkRepr::Text, pool: None }
                }
            };

            // ここまででkind(この列の型と、型ごとの追加設定)が決まった。
            // 続けて、型に関係なく全列共通のnull_rate/uniqueの設定を確認していく
            let null_rate = c.null_rate.unwrap_or(0.0);
            // (0.0..=1.0).contains(&null_rate)は「null_rateが0.0以上1.0以下の範囲に
            // 収まっているか」を調べる書き方。頭に!を付けているので「範囲外なら」という判定になる
            if !(0.0..=1.0).contains(&null_rate) {
                return Err(format!(
                    "列 \"{}\": null_rate({})は0.0〜1.0の範囲で指定してください",
                    c.name, null_rate
                )
                .into());
            }

            let unique = c.unique.unwrap_or(false);
            if unique {
                // matches!(kind, パターン)は「kindがそのパターンに一致するかどうか」を
                // true/falseで返すマクロ(unique_capacityの説明にも出てくる考え方と同じ)
                if matches!(kind, PreparedColumnType::ForeignKey { .. }) {
                    return Err(format!(
                        "列 \"{}\": foreign_key列には unique を指定できません(1つの親の値を複数の子行が参照するのが外部キーの通常の挙動です)",
                        c.name
                    )
                    .into());
                }
                if null_rate > 0.0 {
                    return Err(format!(
                        "列 \"{}\": unique と null_rate は同時に指定できません",
                        c.name
                    )
                    .into());
                }
                // unique_capacity(&kind)が返した値ごとに、エラーにするかどうかを判定する。
                // "パターン if 条件"は「そのパターンに一致し、かつ条件も満たす場合だけ」実行される
                // という書き方(ガード条件と呼ぶ)。上から順に調べ、最初に一致した行だけが実行される
                match unique_capacity(&kind) {
                    UniqueCapacity::Unsupported => {
                        return Err(format!(
                            "列 \"{}\": このtypeはuniqueに対応していません(enum/boolean/gender/blood_type/integer/date/birth_date/name_ja/last_name_ja/first_name_ja/romaji_name/katakana_last_name/katakana_first_name/prefecture_ja/company_name_ja/department_ja/job_title_ja/credit_card_expiry/phone_ja/phone_ja_landline/postal_code/address_ja/uuid/ip_address/jwt/api_key/username/password/profile_image_url/credit_card_number/bank_account_number/product_sku/my_numberのみ対応)",
                            c.name
                        )
                        .into());
                    }
                    UniqueCapacity::Enumerable(capacity) if capacity > UNIQUE_CAPACITY_CAP => {
                        return Err(format!(
                            "列 \"{}\": unique: 値の組み合わせが{}通りあり、上限({}通り)を超えています。範囲や選択肢を絞ってください",
                            c.name, capacity, UNIQUE_CAPACITY_CAP
                        )
                        .into());
                    }
                    // "パターンA | パターンB"は「AまたはBのどちらかに一致すれば」という意味。
                    // EnumerableでもRetryでも、組み合わせ数(capacity)がrow_countより少なければ
                    // 同じエラーにする、という2つのケースをまとめて書いている
                    UniqueCapacity::Enumerable(capacity) | UniqueCapacity::Retry(capacity)
                        if capacity < schema.row_count as u128 =>
                    {
                        return Err(format!(
                            "列 \"{}\": unique: 値の組み合わせが{}通りしかなく、row_count({})分のユニークな値を用意できません",
                            c.name, capacity, schema.row_count
                        )
                        .into());
                    }
                    // 上のどのエラー条件にも当てはまらなかった場合(問題なし)。
                    // {}は「何もしない」という意味で、そのままmatchを抜けてこの下の行に進む
                    UniqueCapacity::Enumerable(_) | UniqueCapacity::Retry(_) => {}
                }
            }

            // 実際の値(unique_pool)はこの後base_seedが決まってから resolve_unique_pools で埋める。
            // Ok(...)で「この1列分の変換に成功した」ことを表す
            Ok(PreparedColumn { name: c.name.clone(), kind, null_rate, unique, unique_pool: None })
        })
        // ここまでの.map(...)は「Result<PreparedColumn, エラー>」を1列ごとに作るところまでだった。
        // collect::<Result<Vec<_>, _>>()は、そのResultの一覧をまとめて1つのResultにする特別な
        // 変換で、「全部がOkならOk(Vec<PreparedColumn>)に、1つでもErrがあれば最初のErrをそのまま
        // 返す」という動きをする(1列でも設定ミスがあれば、そこで処理全体を打ち切れる)
        .collect::<Result<Vec<_>, _>>()
        // inspectは「中身がOkのときだけ、値を変えずに追加の処理(ここでは警告メッセージの表示)を
        // 行い、そのまま同じ値を返す」メソッド。Errのときは何もせずそのまま素通りする
        .inspect(|columns| {
            for warning in misplaced_city_ja_warnings(columns) {
                eprintln!("{warning}");
            }
            for warning in misplaced_katakana_name_warnings(columns) {
                eprintln!("{warning}");
            }
        })
}

// city_ja列は「自分より前にあるprefecture_ja列」しか見ない設計になっている。
// なのでprefecture_ja列は定義してあるのにcity_jaより後ろにある場合、ユーザーの意図
// (都道府県と市区町村を対応させたい)を満たせないまま黙って無関係な市区町村が
// 選ばれてしまう。エラーにするほどではない(単独でcity_jaを使うのは正当な用途)ため、
// 気づけるように警告文を作る(実際に表示するのは呼び出し元)。
fn misplaced_city_ja_warnings(columns: &[PreparedColumn]) -> Vec<String> {
    columns
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.kind, PreparedColumnType::CityJa))
        .filter(|(city_idx, _)| {
            let has_preceding_prefecture =
                columns[..*city_idx].iter().any(|c| matches!(c.kind, PreparedColumnType::PrefectureJa));
            let has_following_prefecture =
                columns[*city_idx + 1..].iter().any(|c| matches!(c.kind, PreparedColumnType::PrefectureJa));
            !has_preceding_prefecture && has_following_prefecture
        })
        .map(|(_, city_col)| {
            format!(
                "警告: 列 \"{}\"(city_ja)より後ろに prefecture_ja 列があります。city_jaは自分より前のprefecture_ja列しか参照しないため、都道府県と市区町村が対応しません。prefecture_ja列をcity_ja列より前に移動してください。",
                city_col.name
            )
        })
        .collect()
}

// katakana_name列も、city_ja列と同様に「自分より前にあるname_ja列」しか見ない設計。
// name_ja列は定義してあるのにkatakana_nameより後ろにある場合、氏名とフリガナが
// 対応しないまま黙って無関係な値が選ばれてしまうため、警告文を作る。
// katakana_name/katakana_name_hankaku/romaji_nameはいずれも「自分より前にあるname_ja列」
// しか参照しない設計なので、3つまとめて同じ警告ロジックで扱う
fn misplaced_katakana_name_warnings(columns: &[PreparedColumn]) -> Vec<String> {
    fn type_label(kind: &PreparedColumnType) -> &'static str {
        match kind {
            PreparedColumnType::KatakanaName { .. } => "katakana_name",
            PreparedColumnType::KatakanaNameHankaku { .. } => "katakana_name_hankaku",
            PreparedColumnType::RomajiName => "romaji_name",
            _ => unreachable!("直前のfilterでこの3型に絞り込み済み"),
        }
    }

    columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            matches!(
                c.kind,
                PreparedColumnType::KatakanaName { .. }
                    | PreparedColumnType::KatakanaNameHankaku { .. }
                    | PreparedColumnType::RomajiName
            )
        })
        .filter(|(kana_idx, _)| {
            let has_preceding_name =
                columns[..*kana_idx].iter().any(|c| matches!(c.kind, PreparedColumnType::NameJa { .. }));
            let has_following_name =
                columns[*kana_idx + 1..].iter().any(|c| matches!(c.kind, PreparedColumnType::NameJa { .. }));
            !has_preceding_name && has_following_name
        })
        .map(|(_, kana_col)| {
            let label = type_label(&kana_col.kind);
            format!(
                "警告: 列 \"{}\"({label})より後ろに name_ja 列があります。{label}は自分より前のname_ja列しか参照しないため、氏名とフリガナ/ローマ字が対応しません。name_ja列を{label}列より前に移動してください。",
                kana_col.name
            )
        })
        .collect()
}

// unique制約への対応方法。組み合わせ数が少ない型は全部列挙してシャッフルする方式(Enumerable)、
// 組み合わせ数が膨大(だが行数よりは十分多い)型は、値を作っては重複チェックし被ったら
// 作り直す方式(Retry)で対応する。どちらにも当てはまらない型はUnsupported
enum UniqueCapacity {
    Enumerable(u128),
    Retry(u128),
    Unsupported,
}

// 「この列タイプは、値を何通り作れるか」を1つずつ調べて返す関数。
// match(パターンマッチ)はRustの構文で、kindの中身が`PreparedColumnType`のどのバリアント
// (種類)かによって実行する処理を振り分ける。他の言語のswitch文に近いが、Rustでは
// 全ての種類を書ききらないとコンパイルエラーになる(書き漏らしを防げる)。
// 呼び出し元(prepare_columns)は、この関数の戻り値を見て
//   - Enumerable(組み合わせ数) → 全部の値を作ってシャッフルする方式(enumerate_values)が使える
//   - Retry(組み合わせ数)      → 値を作っては重複チェックする方式(build_unique_pool_by_retry)を使う
//   - Unsupported              → unique指定はエラーにする
// のどれで対応するかを決める。
// pattern(組み合わせ数の計算が複雑)・city_ja/katakana_name/katakana_name_hankaku
// (直前の列を参照する型で、uniqueと組み合わせると参照関係の設計が複雑になる)・
// sequence/email(後述の通り、そもそも常に重複しないため不要)は非対応のまま。
fn unique_capacity(kind: &PreparedColumnType) -> UniqueCapacity {
    match kind {
        // true/falseの2通りしかない
        PreparedColumnType::Boolean => UniqueCapacity::Enumerable(2),
        // "男性"/"女性"の2通りしかない
        PreparedColumnType::Gender => UniqueCapacity::Enumerable(2),
        // "A型"/"O型"/"B型"/"AB型"の4通りしかない
        PreparedColumnType::BloodType => UniqueCapacity::Enumerable(4),
        // 常に同じ文字列しか返さないため、組み合わせは1通り(row_count=1のときだけunique指定に意味がある。
        // 2行以上を指定すると、他のEnumerable型と同じ「組み合わせが足りない」エラーで自然に弾かれる)
        PreparedColumnType::Fixed { .. } => UniqueCapacity::Enumerable(1),
        // floatは他の型と違い、範囲(min〜max)と桁数(decimals)によって組み合わせ数が
        // 大きく変わる(小数点以下0桁なら少なく、10桁なら莫大になる)ため、その場で計算する。
        PreparedColumnType::Float { min, max, decimals } => {
            // 10f64.powi(2) は「10の2乗」を計算するRustの書き方(10 * 10 = 100になる)。
            // 例えばdecimals=2(小数点以下2桁)なら、値を100倍すれば整数として扱える
            let scale = 10f64.powi(*decimals as i32);
            // 実際の計算の流れ:
            //   1. (max - min) で範囲の幅を求める(例: min=0, max=1 なら幅は1)
            //   2. scaleを掛けて「刻み幅がいくつ入るか」を求める(幅1 × scale100 = 100個分)
            //   3. round()で小数の誤差を丸め、u128(0以上の整数)に変換する
            //   4. +1するのは、両端(minとmax自身)を含めるため(例: 0,1,2のように3個なら幅は2だが個数は3)
            // 「as u128」はRustの型変換の書き方で、小数(f64)を整数(u128)に変換する。
            // Rustのこの変換は特殊な安全設計になっていて、変換できない値(NaNや、
            // u128で表せないほど大きい/小さい値)が来てもエラーで止まらず、
            // 0またはu128の最大値に丸め込まれる(「飽和する」という)。そのため、
            // 極端なmin/max/decimalsの組み合わせで計算結果がおかしくなっても、
            // プログラムが落ちることはなく、安全側(Retry方式)に倒れるだけで済む
            let capacity = (((*max - *min) * scale).round() as u128).saturating_add(1);
            // 組み合わせ数が少なければ全部列挙する方式(Enumerable)、
            // 多ければ値を作っては重複チェックする方式(Retry)を選ぶ
            if capacity <= UNIQUE_CAPACITY_CAP {
                UniqueCapacity::Enumerable(capacity)
            } else {
                UniqueCapacity::Retry(capacity)
            }
        }
        // min〜maxの整数の個数(例: min=1, max=10なら10通り)
        PreparedColumnType::Integer { min, max } => {
            UniqueCapacity::Enumerable((*max as i128 - *min as i128 + 1) as u128)
        }
        // start_days(開始日)からspan_days(日数の幅)日分の、選べる日付の個数
        PreparedColumnType::Date { span_days, .. } => UniqueCapacity::Enumerable(*span_days as u128 + 1),
        // birth_dateはdateと同じ「start_days〜start_days+span_days」の日数分の組み合わせを持つ
        PreparedColumnType::BirthDate { span_days, .. } => UniqueCapacity::Enumerable(*span_days as u128 + 1),
        // choicesに書かれた選択肢の個数がそのまま組み合わせ数になる。
        // unique:trueのときは(重み付けの有無にかかわらず)全選択肢を重複なく列挙するので、
        // weightsは意味を持たない(README/CLAUDE.mdに明記。エラーにはせず単に無視する)
        PreparedColumnType::Enum { choices, .. } => UniqueCapacity::Enumerable(choices.len() as u128),
        // 姓の辞書(LAST_NAMES)の件数 × 名の辞書(FIRST_NAMES)の件数が、作れるフルネームの総数になる
        // (掛け算になるのは、姓と名それぞれを独立に選んで組み合わせるため。例:姓3種×名2種=6通り)
        PreparedColumnType::NameJa { .. } => {
            UniqueCapacity::Enumerable((LAST_NAMES.len() * FIRST_NAMES.len()) as u128)
        }
        // 姓の辞書に載っている件数がそのまま組み合わせ数
        PreparedColumnType::LastNameJa => UniqueCapacity::Enumerable(LAST_NAMES.len() as u128),
        // 名の辞書に載っている件数がそのまま組み合わせ数
        PreparedColumnType::FirstNameJa => UniqueCapacity::Enumerable(FIRST_NAMES.len() as u128),
        // ローマ字の姓辞書 × ローマ字の名辞書(name_jaと同じ掛け算の考え方)
        PreparedColumnType::RomajiName => {
            UniqueCapacity::Enumerable((LAST_NAMES_ROMAJI.len() * FIRST_NAMES_ROMAJI.len()) as u128)
        }
        PreparedColumnType::KatakanaLastName => UniqueCapacity::Enumerable(LAST_NAMES_KANA.len() as u128),
        PreparedColumnType::KatakanaFirstName => UniqueCapacity::Enumerable(FIRST_NAMES_KANA.len() as u128),
        // 都道府県の辞書(CITIES_BY_PREFECTURE)に載っている都道府県の数
        PreparedColumnType::PrefectureJa => UniqueCapacity::Enumerable(CITIES_BY_PREFECTURE.len() as u128),
        // 姓の辞書 × 会社の種類(COMPANY_SUFFIXES、「商事」「工業」等)の掛け算
        PreparedColumnType::CompanyNameJa => {
            UniqueCapacity::Enumerable((LAST_NAMES.len() * COMPANY_SUFFIXES.len()) as u128)
        }
        // 部署名・役職名の辞書に載っている件数がそのまま組み合わせ数
        PreparedColumnType::DepartmentJa => UniqueCapacity::Enumerable(DEPARTMENTS.len() as u128),
        PreparedColumnType::JobTitleJa => UniqueCapacity::Enumerable(JOB_TITLES.len() as u128),
        // 「今日」から1〜5年後(5通り)×1〜12月(12通り)の60通り
        PreparedColumnType::CreditCardExpiry => UniqueCapacity::Enumerable(60),
        // 携帯電話・固定電話は組み合わせ数(市外局番の数 × 10^8)が膨大でEnumerable方式では
        // 列挙しきれないが、実務で指定されるrow_count(最大100万)に対しては十分すぎるほど
        // 大きいため、Retry方式(値を作って重複チェック)で対応する
        PreparedColumnType::PhoneJa => UniqueCapacity::Retry(PHONE_PREFIXES.len() as u128 * 100_000_000),
        PreparedColumnType::PhoneJaLandline => {
            UniqueCapacity::Retry(PHONE_PREFIXES_LANDLINE.len() as u128 * 100_000_000)
        }
        // 郵便番号(NNN-NNNN): 1000 × 10000 = 1000万通り
        PreparedColumnType::PostalCode => UniqueCapacity::Retry(1000 * 10_000),
        // 住所(1列): (都道府県ごとの市区町村数の合計) × 番地1(1〜19) × 番地2(1〜19)
        PreparedColumnType::AddressJa => {
            // iter()で「(都道府県名, その市区町村一覧)」のペアを1つずつ取り出し、
            // mapで市区町村一覧の件数だけを取り出し、sum()で全都道府県分を合計する
            // (「都道府県ごとの市区町村数」を全部足し合わせて、日本全体の市区町村数にする処理)
            let total_cities: u128 =
                CITIES_BY_PREFECTURE.iter().map(|(_, cities)| cities.len() as u128).sum();
            UniqueCapacity::Retry(total_cities * 19 * 19)
        }
        // UUID v4(ランダムな16バイト=2^128通り)は実質衝突しないほど巨大。u128では2^128自体を
        // 表現できない(最大値は2^128-1)ため、u128::MAXで代用する(row_countとの比較にしか
        // 使わないので、この近似で実用上問題ない)
        PreparedColumnType::Uuid => UniqueCapacity::Retry(u128::MAX),
        // IPv4アドレス(例: 192.168.1.1)は0〜255の数字が4つ並ぶ形なので、組み合わせは
        // 256×256×256×256=256の4乗。".pow(4)"はRustで「4乗する」ときの書き方
        PreparedColumnType::IpAddress => UniqueCapacity::Retry(256u128.pow(4)),
        // JWT風文字列・APIキーは64種類/62種類の文字集合から30文字以上組み立てるため、
        // 正確な組み合わせ数はu128の範囲(約3.4×10^38)を超えてオーバーフローする。
        // 実質衝突しないほど巨大であることが分かれば十分なのでu128::MAXで代用する
        PreparedColumnType::Jwt => UniqueCapacity::Retry(u128::MAX),
        PreparedColumnType::ApiKey => UniqueCapacity::Retry(u128::MAX),
        // ローマ字の名前(20種類)+3桁の数字(1〜999) = 19,980通り
        PreparedColumnType::Username => UniqueCapacity::Retry(FIRST_NAMES_ROMAJI.len() as u128 * 999),
        // 68種類の文字から12文字 = 68^12通り(オーバーフローしない範囲で計算可能)
        PreparedColumnType::Password => UniqueCapacity::Retry(68u128.pow(12)),
        // サイズ3種類 × id(1〜999999) ≈ 300万通り
        PreparedColumnType::ProfileImageUrl => UniqueCapacity::Retry(3 * 999_999),
        // 先頭4固定+検査数字を除いた14桁がランダム = 10^14通り
        PreparedColumnType::CreditCardNumber => UniqueCapacity::Retry(10u128.pow(14)),
        // 7桁のゼロ埋め数字 = 10^7通り
        PreparedColumnType::BankAccountNumber => UniqueCapacity::Retry(10u128.pow(7)),
        // 36種類の文字(英大文字+数字)から8文字 = 36^8通り
        PreparedColumnType::ProductSku => UniqueCapacity::Retry(36u128.pow(8)),
        // 検査数字を除いた11桁がランダム = 10^11通り
        PreparedColumnType::MyNumber => UniqueCapacity::Retry(10u128.pow(11)),
        _ => UniqueCapacity::Unsupported,
    }
}

// UniqueCapacity::Enumerableがこれを超える場合はエラーにする。組み合わせ全部をVecに
// 列挙するので、メモリを使いすぎない(や、あまりに時間がかかりすぎない)ようにするための安全弁。
// Retry方式は組み合わせを列挙しないため、この上限の対象外
const UNIQUE_CAPACITY_CAP: u128 = 2_000_000;

// unique_capacityがEnumerable(組み合わせ数)と判定した列について、実際にありうる値を
// 「1つ残らず全部」文字列のリストとして作る関数。呼び出し元(build_unique_pool)がこのリストを
// シャッフルして先頭からrow_count件を取り出すことで、「重複しないrow_count件の値」が完成する。
// vec![...]はRustで配列(正確にはVec)を作るマクロ。to_string()は数値や&str(文字列の参照)を
// String(所有権を持つ文字列)に変換するメソッドで、それぞれ型を揃えるために必要
fn enumerate_values(kind: &PreparedColumnType) -> Vec<String> {
    match kind {
        PreparedColumnType::Boolean => vec!["true".to_string(), "false".to_string()],
        PreparedColumnType::Gender => vec!["男性".to_string(), "女性".to_string()],
        PreparedColumnType::BloodType => {
            vec!["A型".to_string(), "O型".to_string(), "B型".to_string(), "AB型".to_string()]
        }
        PreparedColumnType::Fixed { value } => vec![value.clone()],
        PreparedColumnType::Float { min, max, decimals } => {
            let scale = 10f64.powi(*decimals as i32);
            // min/maxを整数(scaled)に変換してから1ずつ増やしていくことで、
            // 「0.1を何度も足し算する」ような浮動小数点の蓄積誤差(僅かなズレの積み重ね)を避けている。
            // 最後にscaledをscaleで割って元の小数に戻し、format!でdecimals桁の文字列にする
            let min_scaled = (min * scale).round() as i128;
            let max_scaled = (max * scale).round() as i128;
            // (min_scaled..=max_scaled)は「min_scaledからmax_scaledまでの連続した整数」を
            // 表すRust の Range(範囲)。.map(...)で範囲の各値を1つずつ元の小数の文字列に変換し、
            // .collect()で最後にVec<String>(文字列のリスト)にまとめる
            (min_scaled..=max_scaled)
                .map(|scaled| format!("{:.*}", *decimals as usize, scaled as f64 / scale))
                .collect()
        }
        // min〜maxの範囲を1つずつ取り出し、それぞれ文字列に変換してリストにする
        PreparedColumnType::Integer { min, max } => (*min..=*max).map(|v| v.to_string()).collect(),
        // 0日目(start_days)からspan_days日目までを1日ずつ取り出し、それぞれ日付の文字列に変換する。
        // from_num_days_from_ce_optは「西暦1年1月1日から何日目か」という数値を実際の日付に戻す関数で、
        // Option(値が無いかもしれない型)を返すため、.expect(...)で「無いはずが無い(必ず値がある)」
        // ことを保証しつつ中身を取り出している(もし本当に無ければ、その理由のメッセージで停止する)
        PreparedColumnType::Date { start_days, span_days, format } => (0..=*span_days)
            .map(|offset| {
                format_date(
                    chrono::NaiveDate::from_num_days_from_ce_opt(start_days + offset as i32)
                        .expect("span_daysの範囲内なので必ず有効な日付になる"),
                    *format,
                )
            })
            .collect(),
        // choices(選択肢のリスト)をそのまま複製して返すだけ。clone()はRustで「値を複製する」メソッド
        // (choicesは列の設定に属していて、この関数の戻り値として別に持ち出す必要があるため複製する)
        PreparedColumnType::Enum { choices, .. } => choices.clone(),
        // 姓のインデックス(0番目、1番目、…)を1つずつ取り出し、その姓それぞれについて名の
        // インデックスも1つずつ組み合わせてフルネームを作る、という二重ループに相当する処理。
        // flat_mapは「1つの入力から複数の値を作り、それを1段階平らにしてまとめる」メソッドで、
        // ここでは「1つの姓につき名の数だけフルネームを作る」処理を全ての姓に対して行い、
        // 結果を1つの平らなリストにまとめている(単純なmapだと「リストのリスト」になってしまう)
        PreparedColumnType::NameJa { with_space } => (0..LAST_NAMES.len())
            .flat_map(|last_idx| {
                (0..FIRST_NAMES.len()).map(move |first_idx| format_name(last_idx, first_idx, *with_space))
            })
            .collect(),
        PreparedColumnType::LastNameJa => LAST_NAMES.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::FirstNameJa => FIRST_NAMES.iter().map(|s| s.to_string()).collect(),
        // NameJaと同じ「姓×名の全組み合わせ」をflat_mapで作る考え方(ローマ字表記版)
        PreparedColumnType::RomajiName => (0..LAST_NAMES_ROMAJI.len())
            .flat_map(|last_idx| {
                (0..FIRST_NAMES_ROMAJI.len())
                    .map(move |first_idx| format!("{} {}", LAST_NAMES_ROMAJI[last_idx], FIRST_NAMES_ROMAJI[first_idx]))
            })
            .collect(),
        PreparedColumnType::KatakanaLastName => LAST_NAMES_KANA.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::KatakanaFirstName => FIRST_NAMES_KANA.iter().map(|s| s.to_string()).collect(),
        // birth_dateはdateと全く同じ「start_days〜start_days+span_days」の日数分を列挙するだけ
        PreparedColumnType::BirthDate { start_days, span_days, format } => (0..=*span_days)
            .map(|offset| {
                format_date(
                    chrono::NaiveDate::from_num_days_from_ce_opt(start_days + offset as i32)
                        .expect("span_daysの範囲内なので必ず有効な日付になる"),
                    *format,
                )
            })
            .collect(),
        // 都道府県名のリストをそのまま全部返すだけ(random_prefectureがランダムに1件選ぶのと違い、こちらは全件)。
        // iter()は一覧を1件ずつ取り出す準備をするメソッドで、(pref, _)は「(都道府県名, 市区町村一覧)の
        // ペアのうち都道府県名だけを使い、市区町村一覧は使わない」という意味(_は「使わない値」の印)
        PreparedColumnType::PrefectureJa => {
            CITIES_BY_PREFECTURE.iter().map(|(pref, _)| pref.to_string()).collect()
        }
        // NameJaと同じflat_mapの考え方で、姓×会社の種類(COMPANY_SUFFIXES、「商事」「工業」等)の
        // 全組み合わせ(30×8=240通り)を作る(random_company_nameと同じ「株式会社+姓+会社の種類」の組み立て方)
        PreparedColumnType::CompanyNameJa => LAST_NAMES
            .iter()
            .flat_map(|stem| COMPANY_SUFFIXES.iter().map(move |suffix| format!("株式会社{}{}", stem, suffix)))
            .collect(),
        // 部署名・役職名の辞書(DEPARTMENTS/JOB_TITLES)をそのまま全部返すだけ
        PreparedColumnType::DepartmentJa => DEPARTMENTS.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::JobTitleJa => JOB_TITLES.iter().map(|s| s.to_string()).collect(),
        // random_credit_card_expiryと同じ「今日」基準で、1〜5年後×1〜12月の60通りを列挙する。
        // ここでもflat_mapを使い、「1〜5年後それぞれについて、1〜12月の12パターンを作る」処理を行う
        PreparedColumnType::CreditCardExpiry => {
            use chrono::Datelike;
            let today = chrono::Local::now().date_naive();
            (1..=5)
                .flat_map(|years_ahead| {
                    (1..=12).map(move |month| format!("{:02}/{:02}", month, (today.year() + years_ahead) % 100))
                })
                .collect()
        }
        // ここに来るのは必ずEnumerable方式の型だけ(prepare_columnsが事前に弾いているため)。
        // unreachable!はRustで「絶対に実行されないはずのコード」を明示するマクロで、
        // もし実行されてしまった場合はここに書いたメッセージ付きでプログラムを止める
        _ => unreachable!("UniqueCapacity::Enumerable以外の型はここに来ない(prepare_columnsで弾いている)"),
    }
}

// enumerate方式が使えない(組み合わせ数が膨大な)型向け。値を作っては既出かどうかを
// HashSetでチェックし、被っていたら作り直す方式でrow_count件のユニークな値を集める。
// 処理の流れ:
//   1. 空の「もう使った値の集合(seen)」と「これまでに集めた値の一覧(values)」を用意する
//   2. 値を1個ランダムに作る(generate_value)
//   3. seenに無ければ(=初めて出た値なら)valuesに追加する。既にあれば(=重複なら)捨てる
//   4. valuesがrow_count件集まるまで2〜3を繰り返す
// prepare_columnsで「組み合わせ数 >= row_count」を事前に検証済みであり、かつ対象型は
// 組み合わせ数がrow_count(最大100万)よりずっと多いため、衝突は実務上まれで高速に集まる。
// 試行回数の上限(max_attempts)は「実装のバグ等で無限ループにならない」ための安全弁であり、
// 事前検証を正しく通過している限り実際に到達することはない
fn build_unique_pool_by_retry(kind: &PreparedColumnType, row_count: u32, base_seed: u64, column_salt: u64) -> Vec<String> {
    let mut rng = SmallRng::seed_from_u64(base_seed.wrapping_add(column_salt));
    // HashSetは「値が既に入っているか」をすぐ調べられる集合(数学の集合と同じで、同じ値は1個までしか持てない)。
    // with_capacity(row_count)は「最終的にrow_count件入る見込み」を事前に伝えて、
    // 集合が大きくなるたびに内部で確保し直す無駄を減らすための最適化(無くても動作は変わらない)
    let mut seen = std::collections::HashSet::with_capacity(row_count as usize);
    let mut values = Vec::with_capacity(row_count as usize);
    // saturating_mulは掛け算の結果が上限を超えてもエラーにせず、上限いっぱいの値に丸める掛け算。
    // 「row_countの1000倍」と「10万」を比べて大きい方を試行回数の上限にする
    let max_attempts = (row_count as u64).saturating_mul(1000).max(100_000);

    // 0からmax_attempts回、繰り返す(forはRustの繰り返し構文。"_"は「回数は使うが値自体は使わない」の印)
    for _ in 0..max_attempts {
        // 必要な件数が集まったら、途中でも繰り返しを打ち切る(break)
        if values.len() == row_count as usize {
            break;
        }
        // 値を1個作る(row_num引数の0はここでは使われない値なので0を渡している)
        let candidate = generate_value(kind, 0, &mut rng);
        // seen.insert(...)は「集合に追加を試み、既に入っていなければtrue、既に入っていればfalseを返す」
        // メソッド。trueのとき(=初めて出た値のとき)だけ、実際の結果一覧(values)にも追加する
        if seen.insert(candidate.clone()) {
            values.push(candidate);
        }
    }

    // assert_eq!は「2つの値が等しいことを確認し、違っていたらエラーメッセージ付きでプログラムを
    // 止める」マクロ。ここでは「本当にrow_count件集まったか」の最終確認をしている
    assert_eq!(
        values.len(),
        row_count as usize,
        "unique値の収集に失敗しました(組み合わせ数の事前検証をすり抜けた可能性があります)"
    );
    values
}

// 列の全候補値をシャッフルして先頭row_count個を取る=「行数分の重複しない値」の完成
// (Enumerable方式)。Retry方式の型はbuild_unique_pool_by_retryに委譲する。
// column_saltは、同じschema内に複数のunique列があるときに、それぞれ違う乱数列になるようにするための値
fn build_unique_pool(kind: &PreparedColumnType, row_count: u32, base_seed: u64, column_salt: u64) -> Vec<String> {
    // matches!は「値が指定したパターンに一致するかどうか」をtrue/falseで返すマクロ。
    // ここでは「unique_capacity(kind)の結果がRetry(中身の数値は何でもよい)かどうか」を調べている
    if matches!(unique_capacity(kind), UniqueCapacity::Retry(_)) {
        return build_unique_pool_by_retry(kind, row_count, base_seed, column_salt);
    }
    // ここから下はEnumerable方式: 全部の候補値を作り(enumerate_values)、シャッフルして
    // 先頭からrow_count件だけを残す(truncateは「指定した件数より後ろを切り捨てる」メソッド)
    let mut values = enumerate_values(kind);
    let mut rng = SmallRng::seed_from_u64(base_seed.wrapping_add(column_salt));
    values.shuffle(&mut rng);
    values.truncate(row_count as usize);
    values
}

// unique指定のある列すべてに対して、実際の値のプールを計算してPreparedColumnに詰める。
// base_seedが決まった後(=prepare_columnsの後)でないと呼べない。
// iter_mut()は「一覧の各要素を、書き換え可能な形で1つずつ取り出す」メソッド、
// enumerate()は「0番目、1番目、…という連番(i)も一緒に取り出す」メソッド
// (連番iはcolumn_saltとして使い、列ごとに違う乱数列にするため)
pub fn resolve_unique_pools(columns: &mut [PreparedColumn], row_count: u32, base_seed: u64) {
    for (i, column) in columns.iter_mut().enumerate() {
        if column.unique {
            column.unique_pool = Some(build_unique_pool(&column.kind, row_count, base_seed, i as u64));
        }
    }
}

// SchemaFile.tablesの1テーブル分を、prepare_columns済みの状態にしたもの
pub struct PreparedTable {
    // 単一テーブル形式(tables:を使っていない)でtable_name未指定ならNone
    pub name: Option<String>,
    pub row_count: u32,
    pub columns: Vec<PreparedColumn>,
}

// SchemaFileの各テーブルにprepare_columnsを適用する
pub fn prepare_tables(file: &SchemaFile) -> Result<Vec<PreparedTable>, Box<dyn std::error::Error>> {
    // dummygen_jp_guiから複数テーブルを直接生成・プレビューする経路はSchemaFileを
    // normalize_schema_fileを経由せず自分で組み立てるため、テーブル名の検証をここでも行う
    // (schema.yaml経由のときはnormalize_schema_fileで既に検証済みだが、二重にチェックしても
    // 実害は無い)。
    if file.multi_table {
        validate_multi_table_names(&file.tables)?;
    }

    file.tables
        .iter()
        .map(|schema| {
            let columns = prepare_columns(schema)?;
            // 単一テーブル形式でforeign_key列が使われていたら、参照先のテーブルが
            // そもそも存在しえないのでここで弾く
            if !file.multi_table
                && let Some(c) = columns.iter().find(|c| matches!(c.kind, PreparedColumnType::ForeignKey { .. }))
            {
                return Err(format!(
                    "列 \"{}\": foreign_key列は tables: 形式のスキーマでのみ使用できます(1テーブルだけのスキーマには参照先のテーブルがありません)",
                    c.name
                )
                .into());
            }
            Ok(PreparedTable { name: schema.table_name.clone(), row_count: schema.row_count, columns })
        })
        .collect()
}

// (テーブル名, 列名) をキーにした対応表。referencedは「参照元の説明」、
// key_poolsは「実際に生成された値のプール」を持つ
pub type ColumnKey = (String, String);

// 全FK列(foreign_key型の列。他のテーブルの値を参照する列)の参照先(テーブル/列の存在)を
// 検証し、次の2つを作って返す関数:
//   - deps: 依存関係の一覧。deps[子テーブルの番号] に「親テーブルの番号」が並ぶ
//     (例: ordersがusersを参照するなら、deps[ordersの番号] に usersの番号 が入る)
//   - referenced: 「どの(テーブル名, 列名)が、他のどこかから参照されているか」の対応表
//     (後で親テーブル生成後に、その列の値をプール化して子テーブルに渡すために使う)
// pub type ColumnKey = (String, String) は、この関数より前で定義されている型で、
// (テーブル名, 列名)という組をColumnKeyという名前で扱えるようにしたもの
#[allow(clippy::type_complexity)] // (Vec<Vec<usize>>, HashMap<...>) は内部専用の戻り値で、これ以上分ける必要は薄い
pub fn resolve_foreign_keys(
    tables: &mut [PreparedTable],
) -> Result<(Vec<Vec<usize>>, HashMap<ColumnKey, String>), Box<dyn std::error::Error>> {
    // 「テーブル名」から「そのテーブルが tables の何番目にあるか」を引ける対応表を先に作る
    // (このあと参照先のテーブルを名前で探すたびに毎回全部を見て回らずに済むようにするため)。
    // filter_mapは「各要素を変換しつつ、Noneを返したものは結果から除外する」メソッド。
    // t.name.as_deref().map(|n| (n, i))は「テーブル名があれば(名前, 番号)のペアにする、
    // 名前(Option)が無ければNoneのまま」という変換
    let name_to_index: HashMap<&str, usize> = tables
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.name.as_deref().map(|n| (n, i)))
        .collect();

    // vec![Vec::new(); tables.len()]は「空のVecを、テーブルの数だけ並べたVec」を作る書き方
    // (深さ2重のリストの入れ物を用意している)
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); tables.len()];
    let mut referenced: HashMap<ColumnKey, String> = HashMap::new();

    // 全テーブルの、全列を1つずつ調べていく(二重のfor文)
    for (child_idx, table) in tables.iter().enumerate() {
        let child_name = table.name.as_deref().unwrap_or("");
        for column in &table.columns {
            // "let パターン = 式 else { ... };" は、if letの逆で「パターンに一致しなければ
            // elseの中を実行して、その後の処理を打ち切る(ここではcontinueで次の列に進む)」
            // という構文。つまりこの行は「foreign_key型の列でなければ、この列は無視して
            // 次の列へ進む」という意味になる(一致すればref_table/ref_columnを変数として使える)
            let PreparedColumnType::ForeignKey { ref_table, ref_column, .. } = &column.kind else {
                continue;
            };

            if ref_table == child_name {
                return Err(format!(
                    "テーブル \"{}\" の列 \"{}\" が自分自身のテーブルを参照しています。自己参照は未対応です",
                    child_name, column.name
                )
                .into());
            }

            // "let Some(&値) = 式 else { ... };" も同じ考え方で、「name_to_indexに
            // ref_tableという名前のテーブルが見つかればparent_idxとして取り出す、
            // 見つからなければエラーで終了する」という処理。&が付いているのは、
            // get(...)がparent_idxへの参照(&usize)を返すため、それを普通のusizeの
            // 値として取り出すためのパターン
            let Some(&parent_idx) = name_to_index.get(ref_table.as_str()) else {
                return Err(format!(
                    "列 \"{}\": 参照先のテーブル \"{}\" が tables に定義されていません",
                    column.name, ref_table
                )
                .into());
            };

            let parent = &tables[parent_idx];
            // find(...)は「条件を満たす最初の要素を探す」メソッドで、見つからなければNoneを返す。
            // ok_or_else(...)はそのOptionを、Noneのときだけ指定したエラーに変換するメソッド
            // (ok_orとの違いは、エラーメッセージを「実際に必要になったときだけ」作る点)
            let parent_column = parent.columns.iter().find(|c| &c.name == ref_column).ok_or_else(|| {
                format!("列 \"{}\": テーブル \"{}\" に列 \"{}\" がありません", column.name, ref_table, ref_column)
            })?;

            if parent_column.null_rate > 0.0 {
                return Err(format!(
                    "列 \"{}\": 参照先の \"{}.{}\" には null_rate が指定されています。外部キーの参照先にNULLが混ざる列は指定できません",
                    column.name, ref_table, ref_column
                )
                .into());
            }

            // 同じ親を重複して記録しないようにcontainsで確認してから追加する
            // (1つの子テーブルが同じ親テーブルの複数の列を参照していても、依存関係としては1回でよい)
            if !deps[child_idx].contains(&parent_idx) {
                deps[child_idx].push(parent_idx);
            }
            // entry(キー).or_insert_with(作る関数)は「そのキーが既にあれば何もしない、
            // 無ければ関数を実行してその結果を新しい値として登録する」というHashMapの
            // 操作方法。ここでは「同じ列が複数の子から参照されていても、説明文は
            // 最初に見つかったものだけを残す」という意味になる
            referenced
                .entry((ref_table.clone(), ref_column.clone()))
                .or_insert_with(|| format!("{}.{}", child_name, column.name));
        }
    }

    Ok((deps, referenced))
}

// 複数テーブルを「親を必ず子より先に生成できる順番」に並べ替える関数。
// Kahnのアルゴリズムという有名な手法を使っている。考え方はこう:
//   1. 各テーブルについて「自分がいくつのテーブルから必要とされているか(入次数)」を数える
//   2. 入次数が0のテーブル(誰からも先に生成される必要がないテーブル=親を持たないテーブル)
//      から順に「生成してよい」とみなし、そのテーブルを必要としていた側の入次数を1減らす
//   3. 入次数が0になったテーブルを次々追加していき、最終的に全テーブル分並べば完成
// 循環していたら(AがBを必要とし、BもAを必要とする、のような堂々巡り)、具体的な
// 循環パスを1つ再構成してエラーにする
pub fn topological_order(
    deps: &[Vec<usize>],
    tables: &[PreparedTable],
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let n = tables.len();
    // in_degree[i]は「テーブルiが依存している(参照している)相手の数」ではなく、Kahnの
    // アルゴリズムの定義に合わせて「テーブルiに向かう辺の数」、つまり「テーブルiがまだ
    // 生成できるようになるために、先に生成し終えていないといけない親の残り数」を表す
    let mut in_degree = vec![0usize; n];

    // deps[child] = [親のindex...] なので、親→子の辺リスト(children[親] = [子...])を作る
    // (in_degree[i] は「iに向かう辺の数」というKahnの通常の定義に合わせる)
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (child, parents) in deps.iter().enumerate() {
        for &parent in parents {
            children[parent].push(child);
            in_degree[child] += 1;
        }
    }

    // VecDequeは「先頭からも末尾からも出し入れできるリスト」(キュー/待ち行列として使う)。
    // まず「入次数が0のテーブル(=誰も先に待つ必要が無いテーブル)」を全部キューに入れる
    let mut queue: std::collections::VecDeque<usize> =
        (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    // pop_front()は「キューの先頭から1つ取り出す(無ければNone)」メソッド。
    // "while let Some(i) = ... "は「取り出せる限り繰り返す」という構文
    while let Some(i) = queue.pop_front() {
        // このテーブル(i)はもう生成してよい状態なので、結果の並び順(order)に追加する
        order.push(i);
        // iを親として必要としていた子テーブルたちについて、「まだ待っている親の数」を1減らす。
        // 0になった(=もう待つ親がいなくなった)子テーブルは、次にキューへ追加して処理対象にする
        for &child in &children[i] {
            in_degree[child] -= 1;
            if in_degree[child] == 0 {
                queue.push_back(child);
            }
        }
    }

    if order.len() != n {
        // 循環しているテーブル(in_degreeが0にならなかったもの)から、具体的な循環パスを1つたどる
        let remaining: Vec<usize> = (0..n).filter(|&i| !order.contains(&i)).collect();
        let mut path = vec![remaining[0]];
        loop {
            let current = *path.last().unwrap();
            let next = deps[current].iter().find(|p| remaining.contains(p)).copied().unwrap();
            if let Some(pos) = path.iter().position(|&x| x == next) {
                path = path[pos..].to_vec();
                break;
            }
            path.push(next);
        }
        let names: Vec<&str> =
            path.iter().map(|&i| tables[i].name.as_deref().unwrap_or("")).collect();
        return Err(format!(
            "テーブル間の外部キーが循環しています: {} → {}。どこかの参照を外してください",
            names.join(" → "),
            names.first().unwrap_or(&"")
        )
        .into());
    }

    Ok(order)
}

// FK列(foreign_key型の列)が「参照先の値をSQL/JSON/Excelでどう表現すべきか」
// (数値なのか文字列なのか等、reprと呼ぶ)を確定させる関数。
// トポロジカル順(親→子の順)に走査するのがポイントで、多段参照(c.b_id → b.a_id → a.id)の
// ようなケースで、まずbのreprを確定させてからでないとcのreprが正しく決められないため
pub fn resolve_fk_reprs(
    tables: &mut [PreparedTable],
    order: &[usize],
) -> Result<(), Box<dyn std::error::Error>> {
    for &i in order {
        // (子テーブル内の列index, 新しいrepr) を先に集めてから書き込む
        // (同じtables[i]の中で複数のFK列があっても、他のテーブルは参照しないのでここは1テーブル完結)。
        // ここで一旦updatesに集めてから後で書き込む理由: tables[i]の列を読みながら
        // 同時に書き換えようとすると、Rustの「同じデータを同時に借用できない」という
        // 安全のためのルールに触れてしまうため、読む処理と書く処理を分けている
        let mut updates = Vec::new();
        for (col_idx, column) in tables[i].columns.iter().enumerate() {
            if let PreparedColumnType::ForeignKey { ref_table, ref_column, .. } = &column.kind {
                // position(...)は「条件を満たす最初の要素が何番目にあるか」を返すメソッド。
                // expect(...)は「Noneだったら、このメッセージを表示してプログラムを止める」
                // という意味で、ここでは「resolve_foreign_keysで存在確認済みだから、
                // 絶対にSomeになるはず」という前提で使っている
                let parent_idx = tables
                    .iter()
                    .position(|t| t.name.as_deref() == Some(ref_table.as_str()))
                    .expect("resolve_foreign_keysで存在確認済み");
                let parent_column = tables[parent_idx]
                    .columns
                    .iter()
                    .find(|c| &c.name == ref_column)
                    .expect("resolve_foreign_keysで存在確認済み");
                updates.push((col_idx, fk_repr_of(&parent_column.kind)));
            }
        }
        // 集めておいたupdatesを1つずつ書き込む。"&mut tables[i].columns[col_idx].kind"は
        // 「書き換え可能な参照」を取り出す書き方で、*r = repr は「参照先の値そのものを
        // 新しい値で上書きする」という意味(*は「参照が指す先の値そのもの」を表す記号)
        for (col_idx, repr) in updates {
            if let PreparedColumnType::ForeignKey { repr: r, .. } = &mut tables[i].columns[col_idx].kind {
                *r = repr;
            }
        }
    }
    Ok(())
}

// テーブルごとに乱数シードをずらす。table_indexは宣言順index(トポロジカル順ではない)。
// table_index==0のとき必ずbase_seedそのものになるため、単一テーブルの出力は
// 今まで通り1バイトも変わらない。同じ列構成の2テーブルが同一データにならないようにする目的。
// wrapping_mul/wrapping_addは、掛け算・足し算の結果が桁あふれしてもエラーにせず
// 続行する計算方法(row_rngのwrapping_addと同じ考え方)。掛けている大きな16進数の定数は
// 「異なるtable_indexで、なるべく似ていない値になるように」選ばれた適当な大きい数
pub fn table_seed(base_seed: u64, table_index: usize) -> u64 {
    base_seed.wrapping_add((table_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

// 親テーブルを生成し終えた直後に呼ばれる関数。「他のテーブルから参照されている列」
// (referencedに載っている列)の実際の値を全部集めて、pools(プールの置き場)に保存する。
// 子テーブルのforeign_key列はこのプールからランダムに1つ選ぶことで、
// 「必ず親テーブルに実在する値」になることが保証される
pub fn collect_key_pools(
    table: &PreparedTable,
    rows: &[Vec<Option<String>>],
    referenced: &HashMap<ColumnKey, String>,
    pools: &mut HashMap<ColumnKey, Arc<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // "let Some(x) = 式 else { ... };"は前に出てきた「一致しなければelseで打ち切る」構文。
    // ここでは「テーブル名が無ければ(単一テーブル形式なら普通は無い想定だが)、
    // 何もせず正常終了する」という意味
    let Some(table_name) = table.name.as_deref() else {
        return Ok(());
    };

    for (col_idx, column) in table.columns.iter().enumerate() {
        let key = (table_name.to_string(), column.name.clone());
        let Some(referenced_by) = referenced.get(&key) else {
            continue;
        };

        // 生成済みの全行から、この列(col_idx番目)の値だけを取り出して集める。
        // filter_mapは「Noneを除外しつつ、Someの中身だけを取り出す」メソッド
        // (NULLの行はプールに含めない、という意味になる)
        let values: Vec<String> = rows.iter().filter_map(|row| row[col_idx].clone()).collect();
        if values.is_empty() {
            return Err(format!(
                "テーブル \"{}\" の列 \"{}\" に値が1つもないため、これを参照する {} の値を決められません(row_count が0になっていないか確認してください)",
                table_name, column.name, referenced_by
            )
            .into());
        }

        // Arc(Atomically Reference Counted、スレッド間で安全に共有できる参照カウント式の
        // ポインタ)で包んで保存する。同じ親列を複数の子テーブルが参照する場合でも、
        // 実際のデータ(values)を複製せずに1つだけ持ち、みんなで参照を共有できる
        pools.insert(key, Arc::new(values));
    }

    Ok(())
}

// 子テーブルの生成直前に、そのテーブルのFK列(foreign_key型の列)に、対応する親の
// プール(collect_key_poolsで作ったもの)を差し込む関数
pub fn fill_foreign_key_pools(
    columns: &mut [PreparedColumn],
    pools: &HashMap<ColumnKey, Arc<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // iter_mut()は「一覧の各要素を、書き換え可能な形で1つずつ取り出す」メソッド
    for column in columns.iter_mut() {
        if let PreparedColumnType::ForeignKey { ref_table, ref_column, pool, .. } = &mut column.kind {
            let key = (ref_table.clone(), ref_column.clone());
            // cloned()はArc(前述の共有ポインタ)の「参照カウントを1増やして複製する」
            // メソッドで、中身のVec自体はコピーしない(軽い操作)
            let found = pools.get(&key).cloned().expect("トポロジカル順に生成しているので親のプールは必ず存在する");
            *pool = Some(found);
        }
    }
    Ok(())
}

// 複数テーブルをトポロジカル順(親→子)に1テーブルずつ生成する。処理の流れ:
//   1. orderの順番(必ず親が先)で、テーブルを1つずつ処理する
//   2. そのテーブルのFK列に、既に確定した親のプールを差し込む(fill_foreign_key_pools)
//   3. このテーブル専用の乱数シードを作り、unique列のプールを確定させ、全行を生成する
//   4. 生成した行のうち「他のテーブルから参照されている列」があれば、その値をプール化する
//      (collect_key_pools。次以降のテーブルのFK列がこのプールを使えるようにするため)
// main.rs(CLI)とdummygen_jp_guiのTauriコマンドの両方から呼べるよう、
// 元々main.rs内にだけ書かれていたループをここに切り出したもの(ロジックは変更していない)。
// on_table_startは「今どのテーブルを生成中か」を呼び出し側に知らせるコールバック
// (CLIはeprintln!、GUIはTauriの進捗イベントを想定)。
pub fn generate_multi_table_rows(
    tables: &mut [PreparedTable],
    order: &[usize],
    referenced: &HashMap<ColumnKey, String>,
    base_seed: u64,
    mut on_table_start: impl FnMut(usize, &PreparedTable),
) -> Result<Vec<Option<Vec<Vec<Option<String>>>>>, Box<dyn std::error::Error>> {
    let mut key_pools: HashMap<ColumnKey, Arc<Vec<String>>> = HashMap::new();
    // 戻り値の入れ物を先に用意する。まだどのテーブルも生成していないので、
    // 全部Noneにしておき、生成し終えたテーブルから順にSome(行データ)で埋めていく
    let mut rows_by_table: Vec<Option<Vec<Vec<Option<String>>>>> = (0..tables.len()).map(|_| None).collect();

    for &i in order {
        on_table_start(i, &tables[i]);

        fill_foreign_key_pools(&mut tables[i].columns, &key_pools)?;

        // table_seedは宣言順index(i)を使う。トポロジカル順ではないので、
        // 単一テーブル(tables.len()==1)のときは常にbase_seedそのものになる
        let seed = table_seed(base_seed, i);
        let row_count = tables[i].row_count;
        resolve_unique_pools(&mut tables[i].columns, row_count, seed);
        let rows = generate_all_rows(row_count, &tables[i].columns, seed);

        collect_key_pools(&tables[i], &rows, referenced, &mut key_pools)?;

        rows_by_table[i] = Some(rows);
    }

    Ok(rows_by_table)
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Encoding {
    #[value(name = "utf8")]
    Utf8,
    #[value(name = "sjis")]
    Sjis,
}

impl std::fmt::Display for Encoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Encoding::Utf8 => write!(f, "utf8"),
            Encoding::Sjis => write!(f, "sjis"),
        }
    }
}

// 姓・名の候補(ダミーデータの多様性を出すため、それぞれ数十件規模にしてある)。
// 実在の人物とは無関係の、日本で一般的な姓・名から選んだもの
const LAST_NAMES: &[&str] = &[
    "佐藤", "鈴木", "高橋", "田中", "伊藤", "渡辺", "山本", "中村", "小林", "加藤", "吉田", "山田", "山口", "松本",
    "井上", "木村", "林", "斎藤", "清水", "山崎", "森", "阿部", "池田", "橋本", "石川", "前田", "藤田", "後藤",
    "岡田", "村上",
];
const FIRST_NAMES: &[&str] = &[
    "翔太", "陽菜", "大輝", "美咲", "健太", "花子", "太郎", "次郎", "直樹", "由美", "恵子", "誠", "明美", "拓也",
    "智子", "大介", "裕子", "修", "麻衣", "健一",
];
// LAST_NAMES/FIRST_NAMESと添字が1対1で対応するカタカナ読み(フリガナ)。
// 同じ添字を使うことで、katakana_name列がname_ja列と同じ氏名の読みを返せるようにしている
const LAST_NAMES_KANA: &[&str] = &[
    "サトウ", "スズキ", "タカハシ", "タナカ", "イトウ", "ワタナベ", "ヤマモト", "ナカムラ", "コバヤシ", "カトウ",
    "ヨシダ", "ヤマダ", "ヤマグチ", "マツモト", "イノウエ", "キムラ", "ハヤシ", "サイトウ", "シミズ", "ヤマザキ",
    "モリ", "アベ", "イケダ", "ハシモト", "イシカワ", "マエダ", "フジタ", "ゴトウ", "オカダ", "ムラカミ",
];
const FIRST_NAMES_KANA: &[&str] = &[
    "ショウタ", "ヒナ", "ダイキ", "ミサキ", "ケンタ", "ハナコ", "タロウ", "ジロウ", "ナオキ", "ユミ", "ケイコ",
    "マコト", "アケミ", "タクヤ", "トモコ", "ダイスケ", "ユウコ", "オサム", "マイ", "ケンイチ",
];
// LAST_NAMES/FIRST_NAMESと添字が1対1で対応するローマ字表記(ヘボン式)。
// カタカナ→ローマ字の自動変換は促音・拗音・長音の扱いが複雑になるため、
// LAST_NAMES_KANA/FIRST_NAMES_KANAと同様に手書きの対応表にしてある
const LAST_NAMES_ROMAJI: &[&str] = &[
    "Sato", "Suzuki", "Takahashi", "Tanaka", "Ito", "Watanabe", "Yamamoto", "Nakamura", "Kobayashi", "Kato",
    "Yoshida", "Yamada", "Yamaguchi", "Matsumoto", "Inoue", "Kimura", "Hayashi", "Saito", "Shimizu", "Yamazaki",
    "Mori", "Abe", "Ikeda", "Hashimoto", "Ishikawa", "Maeda", "Fujita", "Goto", "Okada", "Murakami",
];
const FIRST_NAMES_ROMAJI: &[&str] = &[
    "Shota", "Hina", "Daiki", "Misaki", "Kenta", "Hanako", "Taro", "Jiro", "Naoki", "Yumi", "Keiko",
    "Makoto", "Akemi", "Takuya", "Tomoko", "Daisuke", "Yuko", "Osamu", "Mai", "Kenichi",
];
const PHONE_PREFIXES: &[&str] = &["090", "080", "070"];
// 固定電話番号(phone_ja_landline)用の市外局番。携帯電話番号(PHONE_PREFIXES)とは別に持つ
const PHONE_PREFIXES_LANDLINE: &[&str] = &["03", "06", "052", "011", "092"];
const COMPANY_SUFFIXES: &[&str] = &["商事", "商会", "工業", "産業", "建設", "システム", "フーズ", "物流"];

// department_ja/job_title_ja用の辞書。日本企業で一般的な部署名・役職名から選んだもの
const DEPARTMENTS: &[&str] = &[
    "営業部", "総務部", "人事部", "経理部", "財務部", "企画部", "広報部", "法務部", "情報システム部", "開発部",
    "製造部", "品質管理部", "購買部", "物流部", "マーケティング部", "カスタマーサポート部", "研究開発部", "監査部",
];
const JOB_TITLES: &[&str] = &[
    "代表取締役", "取締役", "執行役員", "本部長", "部長", "次長", "課長", "課長代理", "係長", "主任", "主査",
    "マネージャー", "リーダー", "一般社員", "契約社員", "派遣社員", "顧問",
];

// 全角カタカナ→半角カタカナの対応表。濁点・半濁点付きの文字は半角では
// 基本字+濁点/半濁点の2文字になる。五十音+濁音+半濁音+拗音+長音など、
// 日本語の氏名フリガナで一般的に使われる文字を広くカバーしている。
// LAST_NAMES_KANA/FIRST_NAMES_KANAの辞書を拡充するときは、新しく増えた文字が
// ここに含まれているかも確認すること(hankaku_katakana_table_covers_all_dictionary_charactersで検証)
const KATAKANA_FULL_TO_HALF: &[(char, &str)] = &[
    ('ア', "ｱ"), ('イ', "ｲ"), ('ウ', "ｳ"), ('エ', "ｴ"), ('オ', "ｵ"),
    ('カ', "ｶ"), ('キ', "ｷ"), ('ク', "ｸ"), ('ケ', "ｹ"), ('コ', "ｺ"),
    ('サ', "ｻ"), ('シ', "ｼ"), ('ス', "ｽ"), ('セ', "ｾ"), ('ソ', "ｿ"),
    ('タ', "ﾀ"), ('チ', "ﾁ"), ('ツ', "ﾂ"), ('テ', "ﾃ"), ('ト', "ﾄ"),
    ('ナ', "ﾅ"), ('ニ', "ﾆ"), ('ヌ', "ﾇ"), ('ネ', "ﾈ"), ('ノ', "ﾉ"),
    ('ハ', "ﾊ"), ('ヒ', "ﾋ"), ('フ', "ﾌ"), ('ヘ', "ﾍ"), ('ホ', "ﾎ"),
    ('マ', "ﾏ"), ('ミ', "ﾐ"), ('ム', "ﾑ"), ('メ', "ﾒ"), ('モ', "ﾓ"),
    ('ヤ', "ﾔ"), ('ユ', "ﾕ"), ('ヨ', "ﾖ"),
    ('ラ', "ﾗ"), ('リ', "ﾘ"), ('ル', "ﾙ"), ('レ', "ﾚ"), ('ロ', "ﾛ"),
    ('ワ', "ﾜ"), ('ヲ', "ｦ"), ('ン', "ﾝ"),
    ('ガ', "ｶﾞ"), ('ギ', "ｷﾞ"), ('グ', "ｸﾞ"), ('ゲ', "ｹﾞ"), ('ゴ', "ｺﾞ"),
    ('ザ', "ｻﾞ"), ('ジ', "ｼﾞ"), ('ズ', "ｽﾞ"), ('ゼ', "ｾﾞ"), ('ゾ', "ｿﾞ"),
    ('ダ', "ﾀﾞ"), ('ヂ', "ﾁﾞ"), ('ヅ', "ﾂﾞ"), ('デ', "ﾃﾞ"), ('ド', "ﾄﾞ"),
    ('バ', "ﾊﾞ"), ('ビ', "ﾋﾞ"), ('ブ', "ﾌﾞ"), ('ベ', "ﾍﾞ"), ('ボ', "ﾎﾞ"),
    ('パ', "ﾊﾟ"), ('ピ', "ﾋﾟ"), ('プ', "ﾌﾟ"), ('ペ', "ﾍﾟ"), ('ポ', "ﾎﾟ"),
    ('ッ', "ｯ"), ('ャ', "ｬ"), ('ュ', "ｭ"), ('ョ', "ｮ"), ('ー', "ｰ"),
    ('ヴ', "ｳﾞ"), ('ァ', "ｧ"), ('ィ', "ｨ"), ('ゥ', "ｩ"), ('ェ', "ｪ"), ('ォ', "ｫ"),
];

// 都道府県ごとに「その都道府県に実在する市区町村っぽい名前」を対応させたもの。
// address_ja(1列で住所)と、prefecture_ja + city_ja(2列に分けたとき)の両方で
// このテーブルを共有することで、「東京都なのに市区町村は北海道の地名」のような
// 不自然な組み合わせが起きないようにしている(F4-2: 列間整合性)。
const CITIES_BY_PREFECTURE: &[(&str, &[&str])] = &[
    ("東京都", &["新宿区", "渋谷区", "港区", "台東区", "世田谷区"]),
    ("大阪府", &["中央区", "北区", "天王寺区", "堺市", "豊中市"]),
    ("愛知県", &["中区", "東区", "豊田市", "岡崎市", "一宮市"]),
    ("北海道", &["札幌市中央区", "函館市", "旭川市", "小樽市", "帯広市"]),
    ("福岡県", &["博多区", "北九州市", "久留米市", "大野城市", "春日市"]),
];

// city_ja列だけを単独で(prefecture_ja列との組み合わせなしで)使ったときのために、
// 全都道府県の市区町村をまとめたリストを1回だけ計算しておく
static ALL_CITIES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    CITIES_BY_PREFECTURE.iter().flat_map(|(_, cities)| cities.iter().copied()).collect()
});

// 姓・名それぞれの添字を先に決める(姓→名の順)。random_nameと同じ乱数消費順序を保つことで、
// --seed指定時の出力(既存のテスト・利用者のデータ)が変わらないようにしている。
// katakana_nameが「同じ行のname_ja列と同じ氏名の読み」を作るには、文字列ではなく
// この添字そのものが必要になる(文字列から元の添字を逆引きするのは面倒なため)
fn random_name_indices(rng: &mut impl Rng) -> (usize, usize) {
    (rng.gen_range(0..LAST_NAMES.len()), rng.gen_range(0..FIRST_NAMES.len()))
}

fn format_name(last_idx: usize, first_idx: usize, with_space: bool) -> String {
    let separator = if with_space { " " } else { "" };
    format!("{}{}{}", LAST_NAMES[last_idx], separator, FIRST_NAMES[first_idx])
}

fn random_name(rng: &mut impl Rng, with_space: bool) -> String {
    let (last_idx, first_idx) = random_name_indices(rng);
    format_name(last_idx, first_idx, with_space)
}

// context(前の列のname_ja)がSomeなら、その氏名と同じ添字のカタカナ読みを返す。
// Noneなら独自にランダムな氏名の読みを作る(katakana_name単独使用時のフォールバック)。
// with_spaceはname_jaのformat_nameと同じ意味(trueで姓の読みと名の読みの間に半角スペース)
fn random_katakana_name(rng: &mut impl Rng, context: Option<(usize, usize)>, with_space: bool) -> String {
    let (last_idx, first_idx) = context.unwrap_or_else(|| random_name_indices(rng));
    let separator = if with_space { " " } else { "" };
    format!("{}{}{}", LAST_NAMES_KANA[last_idx], separator, FIRST_NAMES_KANA[first_idx])
}

// katakana_name_hankaku用。姓の読み・名の読みをそれぞれ個別に半角変換してから連結する
// (先に全角のまま連結してto_hankaku_katakanaへ通すと、区切りの半角スペースが
// KATAKANA_FULL_TO_HALFに載っていない文字として空文字に落とされてしまうため)
fn random_katakana_name_hankaku(rng: &mut impl Rng, context: Option<(usize, usize)>, with_space: bool) -> String {
    let (last_idx, first_idx) = context.unwrap_or_else(|| random_name_indices(rng));
    let separator = if with_space { " " } else { "" };
    format!(
        "{}{}{}",
        to_hankaku_katakana(LAST_NAMES_KANA[last_idx]),
        separator,
        to_hankaku_katakana(FIRST_NAMES_KANA[first_idx])
    )
}

// random_katakana_nameと同じ考え方で、直前のname_ja列と同じ氏名のローマ字表記を返す。
// 「姓 名」の順(ヘボン式、例: "Yamada Taro")で、姓と名の間は常にスペースで区切る
fn random_romaji_name(rng: &mut impl Rng, context: Option<(usize, usize)>) -> String {
    let (last_idx, first_idx) = context.unwrap_or_else(|| random_name_indices(rng));
    format!("{} {}", LAST_NAMES_ROMAJI[last_idx], FIRST_NAMES_ROMAJI[first_idx])
}

// 全角カタカナの文字列を半角カタカナに変換する。KATAKANA_FULL_TO_HALFに無い文字は
// 変換結果から落ちる(空文字になる)。辞書(LAST_NAMES_KANA/FIRST_NAMES_KANA)に
// 出現する文字は対応表でカバーしているため、通常の利用では起こらない
fn to_hankaku_katakana(s: &str) -> String {
    s.chars()
        .map(|c| {
            KATAKANA_FULL_TO_HALF
                .iter()
                .find(|(full, _)| *full == c)
                .map(|(_, half)| *half)
                .unwrap_or_default()
        })
        .collect()
}

fn random_last_name(rng: &mut impl Rng) -> String {
    LAST_NAMES[rng.gen_range(0..LAST_NAMES.len())].to_string()
}

fn random_first_name(rng: &mut impl Rng) -> String {
    FIRST_NAMES[rng.gen_range(0..FIRST_NAMES.len())].to_string()
}

// last_name_ja/first_name_jaと同じく、他の列とは対応付けない独立ランダム
fn random_katakana_last_name(rng: &mut impl Rng) -> String {
    LAST_NAMES_KANA[rng.gen_range(0..LAST_NAMES_KANA.len())].to_string()
}

fn random_katakana_first_name(rng: &mut impl Rng) -> String {
    FIRST_NAMES_KANA[rng.gen_range(0..FIRST_NAMES_KANA.len())].to_string()
}

fn random_email(id: u32, domain: &str) -> String {
    format!("user{}@{}", id, domain)
}

fn random_postal_code(rng: &mut impl Rng) -> String {
    format!("{:03}-{:04}", rng.gen_range(0..1000), rng.gen_range(0..10000))
}

fn random_phone(rng: &mut impl Rng) -> String {
    let prefix = PHONE_PREFIXES[rng.gen_range(0..PHONE_PREFIXES.len())];
    format!("{}-{:04}-{:04}", prefix, rng.gen_range(0..10000), rng.gen_range(0..10000))
}

fn random_phone_landline(rng: &mut impl Rng) -> String {
    let prefix = PHONE_PREFIXES_LANDLINE[rng.gen_range(0..PHONE_PREFIXES_LANDLINE.len())];
    format!("{}-{:04}-{:04}", prefix, rng.gen_range(0..10000), rng.gen_range(0..10000))
}

fn random_address(rng: &mut impl Rng) -> String {
    let (pref, cities) = CITIES_BY_PREFECTURE[rng.gen_range(0..CITIES_BY_PREFECTURE.len())];
    let city = cities[rng.gen_range(0..cities.len())];
    format!("{}{}{}-{}", pref, city, rng.gen_range(1..20), rng.gen_range(1..20))
}

fn random_prefecture(rng: &mut impl Rng) -> String {
    CITIES_BY_PREFECTURE[rng.gen_range(0..CITIES_BY_PREFECTURE.len())].0.to_string()
}

// context_prefectureがSome(その行のprefecture_ja列の値)なら、その都道府県に実在する
// 市区町村の中からランダムに選ぶ。None(prefecture_ja列がスキーマに無い等)なら、
// 全都道府県の市区町村からランダムに選ぶ(city_ja単独使用時のフォールバック)
fn random_city(rng: &mut impl Rng, context_prefecture: Option<&str>) -> String {
    if let Some(pref) = context_prefecture
        && let Some((_, cities)) = CITIES_BY_PREFECTURE.iter().find(|(p, _)| *p == pref)
    {
        return cities[rng.gen_range(0..cities.len())].to_string();
    }
    ALL_CITIES[rng.gen_range(0..ALL_CITIES.len())].to_string()
}

// 氏名で使っている姓のリスト(LAST_NAMES)を「創業者の名字っぽい会社名」として再利用する
fn random_company_name(rng: &mut impl Rng) -> String {
    let stem = LAST_NAMES[rng.gen_range(0..LAST_NAMES.len())];
    let suffix = COMPANY_SUFFIXES[rng.gen_range(0..COMPANY_SUFFIXES.len())];
    format!("株式会社{}{}", stem, suffix)
}

// UUID(v4)は本来 uuid::Uuid::new_v4() で作れるが、それだとOS由来の乱数を直接使うため
// --seed で再現できなくなってしまう。ここでは自前のrng(rowごとにシード付き)で
// ランダムな16バイトを作り、それをUUID形式に組み立てることで再現性を保っている
fn random_uuid(rng: &mut impl Rng) -> String {
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes);
    uuid::Builder::from_random_bytes(bytes).into_uuid().to_string()
}

fn random_department(rng: &mut impl Rng) -> String {
    DEPARTMENTS[rng.gen_range(0..DEPARTMENTS.len())].to_string()
}

fn random_job_title(rng: &mut impl Rng) -> String {
    JOB_TITLES[rng.gen_range(0..JOB_TITLES.len())].to_string()
}

fn random_ip_address(rng: &mut impl Rng) -> String {
    format!(
        "{}.{}.{}.{}",
        rng.gen_range(0..=255),
        rng.gen_range(0..=255),
        rng.gen_range(0..=255),
        rng.gen_range(0..=255)
    )
}

// JWTのヘッダー部分({"alg":"HS256","typ":"JWT"})のbase64url表現は毎回同じ内容なので固定文字列にしてある。
// ペイロード・署名は本物の検証はできないが、それっぽい見た目にするためbase64urlの文字集合から
// ランダムに文字を選んで組み立てる(base64クレートは使わず文字集合から直接選ぶだけで十分なため追加しない)
const JWT_HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
const BASE64URL_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn random_base64url_string(rng: &mut impl Rng, len: usize) -> String {
    (0..len).map(|_| BASE64URL_CHARS[rng.gen_range(0..BASE64URL_CHARS.len())] as char).collect()
}

fn random_jwt(rng: &mut impl Rng) -> String {
    let payload_len = rng.gen_range(40..80);
    let payload = random_base64url_string(rng, payload_len);
    let signature = random_base64url_string(rng, 43);
    format!("{JWT_HEADER}.{payload}.{signature}")
}

const API_KEY_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn random_api_key(rng: &mut impl Rng) -> String {
    let body: String = (0..32).map(|_| API_KEY_CHARS[rng.gen_range(0..API_KEY_CHARS.len())] as char).collect();
    format!("sk_{body}")
}

// FIRST_NAMES_ROMAJI(名のローマ字)を小文字にしたもの+3桁の数字。
// name_ja列との連動はしない独立乱数(katakana_last_name等と同じ考え方)
fn random_username(rng: &mut impl Rng) -> String {
    let first = FIRST_NAMES_ROMAJI[rng.gen_range(0..FIRST_NAMES_ROMAJI.len())].to_lowercase();
    let number = rng.gen_range(1..1000);
    format!("{first}{number}")
}

const PASSWORD_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&";

// 本物のパスワードではなく、それっぽい形の12文字のダミー値(random_api_keyと同じ考え方)
fn random_password(rng: &mut impl Rng) -> String {
    (0..12).map(|_| PASSWORD_CHARS[rng.gen_range(0..PASSWORD_CHARS.len())] as char).collect()
}

// プレースホルダー画像サービス(placehold.jp)のURL文字列を組み立てるだけで、
// 実際に画像を取得したりHTTP通信をしたりはしない(あくまで「それらしい形」の文字列)
fn random_profile_image_url(rng: &mut impl Rng) -> String {
    let size = [100, 150, 200][rng.gen_range(0..3)];
    let id: u32 = rng.gen_range(1..999999);
    format!("https://placehold.jp/{size}x{size}.png?id={id}")
}

// Luhnアルゴリズムで検査数字(0-9)を計算する。digitsは検査数字を除いた本体(左から順)。
// 右端(digitsの最後の要素)から数えて奇数番目(1番目, 3番目, ...)の桁を2倍し、
// 2倍した結果が9を超えたら9を引いてから合計する(これが検査数字を末尾に付けたときに
// 偶数番目になる位置)
fn luhn_check_digit(digits: &[u8]) -> u8 {
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = d as u32;
            if i % 2 == 0 {
                let doubled = d * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                d
            }
        })
        .sum();
    ((10 - (sum % 10)) % 10) as u8
}

fn random_credit_card_number(rng: &mut impl Rng) -> String {
    // 先頭1桁は"4"固定(Visa風)にし、残り14桁をランダムにして本体15桁とする
    let mut digits = vec![4u8];
    digits.extend((0..14).map(|_| rng.gen_range(0..10)));
    let check_digit = luhn_check_digit(&digits);
    digits.push(check_digit);
    digits.iter().map(|d| d.to_string()).collect()
}

// クレジットカードの有効期限を"MM/YY"形式で返す。「今日」を基準に1〜5年後の
// ランダムな年+ランダムな月(1〜12)にする(birth_dateと同じく「今日」基準の考え方)
fn random_credit_card_expiry(rng: &mut impl Rng) -> String {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let year = today.year() + rng.gen_range(1..=5);
    let month = rng.gen_range(1..=12);
    format!("{:02}/{:02}", month, year % 100)
}

// 日本の普通預金口座番号を想定した7桁のゼロ埋め数字
fn random_bank_account_number(rng: &mut impl Rng) -> String {
    format!("{:07}", rng.gen_range(0..10_000_000u32))
}

const SKU_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

// 商品SKUらしい"SKU-"+英大文字/数字8文字の文字列(random_api_keyと同じ乱数の使い方)
fn random_product_sku(rng: &mut impl Rng) -> String {
    let body: String = (0..8).map(|_| SKU_CHARS[rng.gen_range(0..SKU_CHARS.len())] as char).collect();
    format!("SKU-{body}")
}

// 個人番号(マイナンバー)の検査数字。行政手続における特定の個人を識別するための
// 番号の利用等に関する法律で定められた計算方法に従う。右から1始まりで数えた位置iの
// 重みは、iが1〜6なら(i+1)、7〜11なら(i-5)。各桁×重みの合計を11で割った余りが
// 0か1なら検査数字は0、それ以外は(11-余り)
fn my_number_check_digit(digits: &[u8; 11]) -> u8 {
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(j, &d)| {
            let i = 11 - j; // 右から1始まりの位置
            let weight = if i <= 6 { i + 1 } else { i - 5 };
            d as u32 * weight as u32
        })
        .sum();
    let remainder = sum % 11;
    if remainder <= 1 { 0 } else { (11 - remainder) as u8 }
}

fn random_my_number(rng: &mut impl Rng) -> String {
    let mut digits = [0u8; 11];
    for d in &mut digits {
        *d = rng.gen_range(0..10);
    }
    let check_digit = my_number_check_digit(&digits);
    let mut all: Vec<u8> = digits.to_vec();
    all.push(check_digit);
    all.iter().map(|d| d.to_string()).collect()
}

// 同じ行の中で、前の列の生成結果を後ろの列に伝えるための文脈。
// 列間で参照し合う列タイプ(prefecture_ja→city_ja、name_ja→katakana_name)が増えたため、
// 個別の引数(context_prefectureなど)を都度増やす代わりに、まとめて1つのstructにしている。
#[derive(Default)]
struct RowContext {
    last_prefecture: Option<String>,
    last_name_indices: Option<(usize, usize)>,
}

// 1列分の値を作る。null_rateの確率でNone(NULL)を返す。
// ctxには「同じ行の、これより前にある列の生成結果」が入っており、
// city_ja列・katakana_name列がそれぞれprefecture_ja列・name_ja列の値を参照するのに使う。
// 戻り値は「(セルの値, name_ja列として新たに選んだ姓・名の添字)」という2つの値のタプル
// (Rustでは複数の値をまとめて返したいとき、こうしたタプル型がよく使われる)。
// 2つ目は「name_ja列として新たに選んだ姓・名の添字」(それ以外の列やNULL・
// unique_pool経由の場合はNone)で、generate_rowがRowContextに保存するために使う。
// 引数のrng: &mut impl Rngは「Rngという機能(トレイト)を持つ何らかの型への、書き換え可能な
// 参照」という意味。呼び出し元がSmallRng等どの乱数生成器を渡しても、この関数は同じように使える
fn generate_cell(
    column: &PreparedColumn,
    row_num: u32,
    rng: &mut impl Rng,
    ctx: &RowContext,
) -> (Option<String>, Option<(usize, usize)>) {
    // uniqueな列は、あらかじめ用意しておいたプールからこの行番号に対応する値を取り出すだけ
    // (unique同士でnull_rateとの併用はprepare_columnsで禁止しているので、Noneになることは無い)。
    // pool[(row_num - 1) as usize]は「1始まりの行番号を0始まりの配列の位置に直してから
    // 値を取り出す」という書き方(row_num=1なら配列の0番目、row_num=2なら1番目、…)
    if let Some(pool) = &column.unique_pool {
        return (Some(pool[(row_num - 1) as usize].clone()), None);
    }

    // gen_bool(確率)は「指定した確率(0.0〜1.0)でtrueを返す」メソッド。
    // null_rateが0より大きく、かつ抽選に当たった場合だけNone(NULL)を返して、
    // この列の値作りをここで打ち切る
    if column.null_rate > 0.0 && rng.gen_bool(column.null_rate) {
        return (None, None);
    }

    // 列タイプに応じて分岐する。上の4つ(CityJa/KatakanaName/KatakanaNameHankaku/RomajiName/
    // NameJa)は「他の列の値を参照する」「後で参照されるための添字を返す」という特別な扱いが
    // 必要なのでここに直接書き、それ以外の型は`_`(それ以外全部、という意味)でgenerate_value
    // にそのまま任せる
    match column.kind {
        PreparedColumnType::CityJa => (Some(random_city(rng, ctx.last_prefecture.as_deref())), None),
        PreparedColumnType::KatakanaName { with_space } => {
            (Some(random_katakana_name(rng, ctx.last_name_indices, with_space)), None)
        }
        PreparedColumnType::KatakanaNameHankaku { with_space } => {
            (Some(random_katakana_name_hankaku(rng, ctx.last_name_indices, with_space)), None)
        }
        PreparedColumnType::RomajiName => (Some(random_romaji_name(rng, ctx.last_name_indices)), None),
        PreparedColumnType::NameJa { with_space } => {
            let (last_idx, first_idx) = random_name_indices(rng);
            (Some(format_name(last_idx, first_idx, with_space)), Some((last_idx, first_idx)))
        }
        _ => (Some(generate_value(&column.kind, row_num, rng)), None),
    }
}

// 1行分(全列)の値を作る。列は前から順番に処理し、prefecture_ja/name_ja列の値を
// RowContextに覚えておいて後ろの列(city_ja/katakana_name)に渡す
// (どちらも「参照される側」の列が「参照する側」の列より前に定義されている必要がある)
fn generate_row(columns: &[PreparedColumn], row_num: u32, rng: &mut impl Rng) -> Vec<Option<String>> {
    // RowContext::default()は「全フィールドを空(None)にした初期状態」を作る
    // (structにderive(Default)が付いているとこのメソッドが自動で使えるようになる)
    let mut ctx = RowContext::default();
    let mut values = Vec::with_capacity(columns.len());

    // 列を先頭から順番に1つずつ処理する(この「順番通り」というのが、prefecture_ja→city_ja
    // のような参照関係が成立するために重要な前提になっている)
    for column in columns {
        let (cell, name_indices) = generate_cell(column, row_num, rng, &ctx);
        // 今処理した列が「後ろの列から参照されうる列」なら、その結果をctxに覚えておく。
        // それ以外の列タイプでは何もしない(`_ => {}`が「何もしない」という意味)
        match column.kind {
            PreparedColumnType::PrefectureJa => ctx.last_prefecture = cell.clone(),
            PreparedColumnType::NameJa { .. } => ctx.last_name_indices = name_indices,
            _ => {}
        }
        values.push(cell);
    }

    values
}

// 列タイプに応じて、1つ分の値を作る。row_numは「今何行目か(1始まり)」
fn generate_value(kind: &PreparedColumnType, row_num: u32, rng: &mut impl Rng) -> String {
    match kind {
        PreparedColumnType::Sequence => row_num.to_string(),
        PreparedColumnType::NameJa { with_space } => random_name(rng, *with_space),
        PreparedColumnType::LastNameJa => random_last_name(rng),
        PreparedColumnType::FirstNameJa => random_first_name(rng),
        PreparedColumnType::Email { domain } => random_email(row_num, domain),
        PreparedColumnType::Integer { min, max } => rng.gen_range(*min..=*max).to_string(),
        PreparedColumnType::Float { min, max, decimals } => {
            let value: f64 = rng.gen_range(*min..=*max);
            format!("{:.*}", *decimals as usize, value)
        }
        PreparedColumnType::Boolean => rng.gen_bool(0.5).to_string(),
        PreparedColumnType::Gender => if rng.gen_bool(0.5) { "男性".to_string() } else { "女性".to_string() },
        // 日本人の血液型分布の目安(A:O:B:AB ≒ 4:3:2:1)で重み付け
        PreparedColumnType::BloodType => {
            let pairs = [("A型", 4.0_f64), ("O型", 3.0), ("B型", 2.0), ("AB型", 1.0)];
            pairs.choose_weighted(rng, |(_, weight)| *weight).unwrap().0.to_string()
        }
        PreparedColumnType::Pattern { pieces } => {
            let mut value = String::new();
            for piece in pieces {
                let repeat =
                    if piece.min_repeat == piece.max_repeat { piece.min_repeat } else { rng.gen_range(piece.min_repeat..=piece.max_repeat) };
                for _ in 0..repeat {
                    value.push(piece.chars[rng.gen_range(0..piece.chars.len())]);
                }
            }
            value
        }
        PreparedColumnType::Date { start_days, span_days, format } => {
            let offset = if *span_days == 0 { 0 } else { rng.gen_range(0..=*span_days) };
            let date = chrono::NaiveDate::from_num_days_from_ce_opt(*start_days + offset as i32)
                .expect("日付の範囲はprepare_columnsで検証済み");
            format_date(date, *format)
        }
        PreparedColumnType::BirthDate { start_days, span_days, format } => {
            let offset = if *span_days == 0 { 0 } else { rng.gen_range(0..=*span_days) };
            let date = chrono::NaiveDate::from_num_days_from_ce_opt(*start_days + offset as i32)
                .expect("年齢範囲から計算した日付は必ず有効な日付になる");
            format_date(date, *format)
        }
        PreparedColumnType::PostalCode => random_postal_code(rng),
        PreparedColumnType::PhoneJa => random_phone(rng),
        PreparedColumnType::PhoneJaLandline => random_phone_landline(rng),
        PreparedColumnType::AddressJa => random_address(rng),
        PreparedColumnType::CompanyNameJa => random_company_name(rng),
        PreparedColumnType::Uuid => random_uuid(rng),
        PreparedColumnType::PrefectureJa => random_prefecture(rng),
        // context(前の列のprefecture_ja/name_ja)が無い状態での単独生成。
        // 文脈付きの生成はgenerate_cellが行う
        PreparedColumnType::CityJa => random_city(rng, None),
        PreparedColumnType::KatakanaName { with_space } => random_katakana_name(rng, None, *with_space),
        PreparedColumnType::KatakanaNameHankaku { with_space } => {
            random_katakana_name_hankaku(rng, None, *with_space)
        }
        // context(前の列のname_ja)が無い状態での単独生成。文脈付きの生成はgenerate_cellが行う
        PreparedColumnType::RomajiName => random_romaji_name(rng, None),
        PreparedColumnType::KatakanaLastName => random_katakana_last_name(rng),
        PreparedColumnType::KatakanaFirstName => random_katakana_first_name(rng),
        PreparedColumnType::DepartmentJa => random_department(rng),
        PreparedColumnType::JobTitleJa => random_job_title(rng),
        PreparedColumnType::IpAddress => random_ip_address(rng),
        PreparedColumnType::Jwt => random_jwt(rng),
        PreparedColumnType::ApiKey => random_api_key(rng),
        PreparedColumnType::Username => random_username(rng),
        PreparedColumnType::Password => random_password(rng),
        PreparedColumnType::ProfileImageUrl => random_profile_image_url(rng),
        PreparedColumnType::CreditCardNumber => random_credit_card_number(rng),
        PreparedColumnType::CreditCardExpiry => random_credit_card_expiry(rng),
        PreparedColumnType::BankAccountNumber => random_bank_account_number(rng),
        PreparedColumnType::ProductSku => random_product_sku(rng),
        PreparedColumnType::MyNumber => random_my_number(rng),
        PreparedColumnType::Enum { choices, weights } => match weights {
            // choose_weighted(rand::seq::SliceRandom、既にuseされている)で重み付き抽選する。
            // prepare_columnsで「個数がchoicesと一致・負の数なし・合計>0」を検証済みなので、
            // ここでのunwrapは失敗しない
            Some(w) => {
                let pairs: Vec<(&String, f64)> = choices.iter().zip(w.iter().copied()).collect();
                pairs.choose_weighted(rng, |(_, weight)| *weight).unwrap().0.clone()
            }
            None => choices[rng.gen_range(0..choices.len())].clone(),
        },
        PreparedColumnType::Fixed { value } => value.clone(),
        PreparedColumnType::ForeignKey { pool, .. } => {
            let pool = pool.as_ref().expect("FKプールはfill_foreign_key_poolsで親テーブル生成後に必ず埋まっている");
            pool[rng.gen_range(0..pool.len())].clone()
        }
    }
}

// SQLのVALUES句に書くとき、文字列として ' ' で囲む必要がある列タイプかどうか
fn is_text_column(kind: &PreparedColumnType) -> bool {
    // foreign_key列は参照先の型(repr)次第でクォート要否が変わるので個別に判定し、
    // それ以外は列タイプで固定的に判定する
    if let PreparedColumnType::ForeignKey { repr, .. } = kind {
        return *repr == FkRepr::Text;
    }
    matches!(
        kind,
        PreparedColumnType::NameJa { .. }
            | PreparedColumnType::LastNameJa
            | PreparedColumnType::FirstNameJa
            | PreparedColumnType::RomajiName
            | PreparedColumnType::KatakanaLastName
            | PreparedColumnType::KatakanaFirstName
            | PreparedColumnType::Email { .. }
            | PreparedColumnType::Date { .. }
            | PreparedColumnType::BirthDate { .. }
            | PreparedColumnType::PostalCode
            | PreparedColumnType::PhoneJa
            | PreparedColumnType::PhoneJaLandline
            | PreparedColumnType::AddressJa
            | PreparedColumnType::CompanyNameJa
            | PreparedColumnType::Uuid
            | PreparedColumnType::PrefectureJa
            | PreparedColumnType::CityJa
            | PreparedColumnType::KatakanaName { .. }
            | PreparedColumnType::KatakanaNameHankaku { .. }
            | PreparedColumnType::DepartmentJa
            | PreparedColumnType::JobTitleJa
            | PreparedColumnType::IpAddress
            | PreparedColumnType::Jwt
            | PreparedColumnType::ApiKey
            | PreparedColumnType::CreditCardNumber
            | PreparedColumnType::CreditCardExpiry
            | PreparedColumnType::BankAccountNumber
            | PreparedColumnType::ProductSku
            | PreparedColumnType::MyNumber
            | PreparedColumnType::Enum { .. }
            | PreparedColumnType::Fixed { .. }
            | PreparedColumnType::Gender
            | PreparedColumnType::BloodType
            | PreparedColumnType::Pattern { .. }
            | PreparedColumnType::Username
            | PreparedColumnType::Password
            | PreparedColumnType::ProfileImageUrl
    )
}

// 進捗バーを表示する行数のしきい値。これより少ない行数だと一瞬で終わってしまい、
// バーを表示してもチラッと見えるだけで邪魔なうえ、cargo testの出力も汚れるので隠す
const PROGRESS_BAR_THRESHOLD: u32 = 1000;

pub fn new_progress_bar(row_count: u32) -> indicatif::ProgressBar {
    if row_count < PROGRESS_BAR_THRESHOLD {
        return indicatif::ProgressBar::hidden();
    }
    let bar = indicatif::ProgressBar::new(row_count as u64);
    bar.set_style(
        indicatif::ProgressStyle::with_template("生成中 [{bar:40.cyan/blue}] {pos}/{len}行 ({percent}%)")
            .expect("テンプレート文字列は固定なので必ずパースできる")
            .progress_chars("=>-"),
    );
    bar
}

// 絶対行番号 start_row..=end_row の範囲だけをrayonで並列生成する。
// write_csv_streaming/write_sql_streaming がチャンク単位で呼び出すための内部ヘルパーで、
// generate_all_rowsとは別に用意している(generate_all_rowsは既存の利用箇所を壊さないよう変更しない)。
// row_numには必ず「ファイル全体を通しての行番号」を渡すこと。チャンクごとに1から
// 振り直すと、row_rngのseed(base_seed+row_num)とunique_poolの添字(row_num-1)の
// 両方がずれてしまい、--seedの再現性とunique制約が静かに壊れる。
fn generate_rows_range(
    columns: &[PreparedColumn],
    base_seed: u64,
    start_row: u32,
    end_row: u32,
) -> Vec<Vec<Option<String>>> {
    // into_par_iter()はrayonライブラリが提供するメソッドで、通常のiter()と違い
    // 「複数のCPUコアに処理を自動で分散して、並列に実行する」イテレータを作る。
    // 1行ごとにrow_rng(base_seed, row_num)で独立した乱数生成器を作っているため、
    // どのスレッドがどの行を処理しても結果が変わらない(row_numが同じなら常に同じ乱数列になる)
    (start_row..=end_row)
        .into_par_iter()
        .map(|row_num| {
            let mut rng = row_rng(base_seed, row_num);
            generate_row(columns, row_num, &mut rng)
        })
        .collect()
}

// 全行・全列の値を、rayonで並列に生成する。CSV/SQL/JSON/Excelどの出力形式でも
// 「値を作る」部分は共通なので、ここに一本化している(進捗バーの更新もここでまとめて行う)。
// ファイルへの書き込み(直列処理が必要)は、それぞれのbuild_*関数が個別に行う。
pub fn generate_all_rows(row_count: u32, columns: &[PreparedColumn], base_seed: u64) -> Vec<Vec<Option<String>>> {
    let progress = new_progress_bar(row_count);

    // 1行目からrow_count行目までを並列に処理する(generate_rows_rangeと同じ考え方)。
    // progress.inc(1)は進捗バーを1つ分だけ進める呼び出しで、複数のスレッドから同時に
    // 呼ばれても安全なように作られている(indicatifライブラリ側の保証)
    let rows: Vec<Vec<Option<String>>> = (1..=row_count)
        .into_par_iter()
        .map(|row_num| {
            let mut rng = row_rng(base_seed, row_num);
            let row = generate_row(columns, row_num, &mut rng);
            progress.inc(1);
            row
        })
        .collect();

    progress.finish_and_clear();
    rows
}

// SQL文字列リテラルの中に ' が含まれていると構文が壊れるので '' に二重化してエスケープする
fn sql_literal(kind: &PreparedColumnType, value: &str) -> String {
    if is_text_column(kind) {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

// テーブル名・列名(識別子)を "..." で囲む。囲まないと、名前に , や " などSQLとして
// 意味を持つ文字が含まれていた場合に文の構造そのものが壊れてしまう
// (例: 列名 "id, name" をそのまま埋め込むと、列の数とVALUESの値の数が食い違ってしまう)
fn sql_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// 一度に書き出す行数(あまり大きいINSERT文1本にまとめると読みにくく、SQLエンジン側の
// 上限に引っかかることもあるため、この件数ごとにINSERT文を分ける)
const SQL_BATCH_SIZE: u32 = 1000;

// write_csv_streaming/write_sql_streaming が1回に処理する行数(チャンクサイズ)の既定値。
// SQL_BATCH_SIZE(1000)の倍数にしておくことで、SQLストリーミング出力のINSERT文の
// 区切り方が一括生成版(build_sql_from_rows)と完全に同じになる(2節参照)。
pub const DEFAULT_CHUNK_SIZE: u32 = 10_000;

// チャンク1つ分のテキストを、指定された文字コードでBufWriterに書き込む共通処理。
// Shift-JISへの変換エラーはチャンクをまたいで呼び出し元の had_sjis_errors に集約し、
// 警告メッセージが複数回出力されてしまわないようにする(write_textの単発版と挙動を揃える)。
fn write_chunk_text(
    out: &mut impl Write,
    text: &str,
    encoding: Encoding,
    had_sjis_errors: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match encoding {
        Encoding::Utf8 => out.write_all(text.as_bytes())?,
        Encoding::Sjis => {
            let (bytes, had_errors) = encode_to_sjis(text);
            if had_errors {
                *had_sjis_errors = true;
            }
            out.write_all(&bytes)?;
        }
    }
    Ok(())
}

// SQLの中身(行データはgenerate_all_rowsで生成済みのものを受け取る)をUTF-8の文字列として
// メモリ上で組み立てる(ファイルにはまだ書かない)。複数形式を同時出力するとき、同じ行データを
// 形式の数だけ重複生成しないよう、「生成」(generate_all_rows)と「清書」(この関数)を分けている。
pub fn build_sql_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    table_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // table_nameが空文字・空白だけ(例: "   ")だと、そのままではINSERT INTO "   "のような
    // 実用上意味のない(データベースによっては構文エラーになる)SQLになってしまうため、
    // ここで一括して弾く。呼び出し元(単一テーブル・複数テーブルどちらも)は全てこの関数を
    // 経由するため、ここ1箇所でチェックすれば十分
    if table_name.trim().is_empty() {
        return Err("SQL出力(--format sql)には、schema.yamlに table_name の指定が必要です".into());
    }

    // 列名を1つずつSQL用に整形(sql_ident)し、joinで「, 」区切りの1本の文字列にまとめる
    // (例: ["id", "name"] → "`id`, `name`"のような形)
    let column_names = columns
        .iter()
        .map(|c| sql_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let table_ident = sql_ident(table_name);

    // 1行ずつ、VALUES句の"(値1, 値2, ...)"の形に組み立てる
    let value_rows: Vec<String> = rows
        .iter()
        .map(|row| {
            // zip(columns)で「セルの値」と「その列の設定(型情報)」を1対1でペアにする
            // (sql_literalが型に応じたクォートの要否を判断するために列の型が必要なため)
            let values: Vec<String> = row
                .iter()
                .zip(columns)
                .map(|(cell, c)| match cell {
                    Some(v) => sql_literal(&c.kind, v),
                    None => "NULL".to_string(), // SQLのNULLはクォートしてはいけない
                })
                .collect();
            format!("({})", values.join(", "))
        })
        .collect();

    // INSERT文の組み立て(バッチ分割)はファイル1本を順番に書くだけなので並列化せず、直列に行う。
    // chunks(SQL_BATCH_SIZE)は「一覧をSQL_BATCH_SIZE件ずつのかたまりに分割する」メソッドで、
    // 1本のINSERT文に含める行数を制限している(1本のSQL文が長くなりすぎるのを防ぐため)。
    // push_strは「文字列の末尾に別の文字列をつなげる」メソッド
    let mut sql = String::new();
    for batch in value_rows.chunks(SQL_BATCH_SIZE as usize) {
        sql.push_str(&format!("INSERT INTO {} ({}) VALUES\n", table_ident, column_names));
        sql.push_str(&batch.join(",\n"));
        sql.push_str(";\n\n");
    }

    Ok(sql)
}

// build_sql_from_rowsの「行数とシードを渡すだけで一発で作れる」版。
// mainではrowsを1回だけ生成して複数形式で使い回すためこの関数は呼ばないが、
// 既存のテストが読みやすいようにこの形のまま残してある(テストからのみ使用)
#[cfg(test)]
fn build_sql(
    row_count: u32,
    columns: &[PreparedColumn],
    table_name: &str,
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    build_sql_from_rows(columns, &generate_all_rows(row_count, columns, base_seed), table_name)
}

// write_csv_streamingのSQL版。chunk_sizeがSQL_BATCH_SIZE(1000)の倍数である限り、
// build_sql_from_rowsをチャンクごとに呼んでも(このチャンクの中でさらに1000行ごとに
// INSERT文を分けるので)、一括生成した場合とINSERT文の区切り方が完全に一致する。
pub fn write_sql_streaming(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
    table_name: &str,
    path: &str,
    encoding: Encoding,
    chunk_size: u32,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    let mut had_sjis_errors = false;
    let total = row_count as u64;
    let mut done: u64 = 0;

    if row_count == 0 {
        on_progress(0, 0);
    }

    for chunk_start in (1..=row_count).step_by(chunk_size as usize) {
        let chunk_end = (chunk_start + chunk_size - 1).min(row_count);
        let rows = generate_rows_range(columns, base_seed, chunk_start, chunk_end);
        let sql_text = build_sql_from_rows(columns, &rows, table_name)?;
        write_chunk_text(&mut out, &sql_text, encoding, &mut had_sjis_errors)?;

        done += (chunk_end - chunk_start + 1) as u64;
        on_progress(done, total);
    }

    out.flush()?;
    if had_sjis_errors {
        eprintln!("警告: Shift-JISに変換できない文字が '?' に置き換えられました");
    }
    Ok(())
}

// CSVの中身(行データは生成済みのものを受け取る)をUTF-8の文字列として組み立てる。
// NULL(None)はCSVでは空文字として書き出す。
// quote_all: trueのとき全ての値をダブルクォートで囲む(write_csv_streamingのquote_allと同じ挙動)。
// falseのとき(既定)はカンマ・改行・"を含む値だけを囲む
pub fn build_csv_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    quote_all: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    // csvクレート(CSVの読み書きをしてくれる外部ライブラリ)のQuoteStyleは「値をいつ""で
    // 囲むか」の設定。Alwaysは常に囲む、Necessaryは「カンマ・改行・"を含む値のときだけ」
    // 囲む(このクレートの標準の挙動)。if式で、quote_allの真偽に応じてどちらを使うか決める
    let quote_style = if quote_all { csv::QuoteStyle::Always } else { csv::QuoteStyle::Necessary };
    // WriterBuilderは「これから使うWriter(書き込み役)の設定を組み立てる」ためのもの。
    // from_writer(Vec::new())で「ファイルではなく、メモリ上の空のバイト列(Vec)に書き込む」
    // writerを作る(すぐファイルに保存せず、まず文字列として組み立てたいため)
    let mut writer = csv::WriterBuilder::new().quote_style(quote_style).from_writer(Vec::new());
    // 各列の名前だけを取り出してVec(配列)にする(1行目=ヘッダー行にするため)
    let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    writer.write_record(&headers)?; // ヘッダー行を書き込む(?は「エラーなら即座に呼び出し元へ返す」構文)

    // rowsに入っている行を1行ずつ順番に処理する
    for row in rows {
        // 1行分のセルを見て、値があればその文字列、無ければ(None=NULL)空文字にする。
        // as_deref()は「Option<String>からOption<&str>を取り出す」変換、
        // unwrap_or("")は「値が無ければ代わりに空文字を使う」という意味
        let record: Vec<&str> = row.iter().map(|cell| cell.as_deref().unwrap_or("")).collect();
        writer.write_record(&record)?;
    }

    let bytes = writer.into_inner()?; // 内部バッファ(UTF-8のバイト列)を取り出す
    Ok(String::from_utf8(bytes)?)
}

// build_csv_from_rowsの「行数とシードを渡すだけで一発で作れる」版(テストからのみ使用)
#[cfg(test)]
fn build_csv(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    build_csv_from_rows(columns, &generate_all_rows(row_count, columns, base_seed), false)
}

// build_csv_from_rowsとの違いは、全行をメモリに載せてから一括で書き出すのではなく、
// chunk_size行ずつ「生成してすぐファイルに書き足す」を繰り返す点(2節: ストリーミング化)。
// 大量行(最大100万行を想定)でも、メモリ上に保持するのは常に1チャンク分だけになる。
// on_progressはチャンクを書き終えるたびに(完了行数, 全行数)で呼ばれる。
// CLI側はindicatif::ProgressBar::set_positionを、GUI側はTauriのイベント発火を
// 渡すことを想定している。
// row_countを一度に全部メモリに載せず、chunk_size行ずつ「生成してすぐファイルに書き足す」を
// 繰り返す関数(処理の中身はbuild_csv_from_rowsと似ているが、こちらは全行を溜め込まない)。
// 大まかな流れ:
//   1. ファイルを開き、必要ならBOM(後述)を書く
//   2. chunk_size行ずつ、範囲を区切って値を生成する(generate_rows_range)
//   3. 生成した分だけCSVの文字列に組み立て、文字コードを変換してファイルに書き足す
//   4. 全部書き終わるまで2〜3を繰り返す
pub fn write_csv_streaming(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
    path: &str,
    encoding: Encoding,
    chunk_size: u32,
    // trueかつUTF-8のときだけ、ファイル先頭にUTF-8のBOM(EF BB BF)を書き込む。
    // 日本語版Excelは、BOMの無いUTF-8のCSVをダブルクリックで開くと(BOMがShift-JISか
    // UTF-8かを判別するヒントになるため)既定の文字コード(多くはShift-JIS)として
    // 読み込んでしまい文字化けする。CLIからの呼び出し(main.rs)は既存の出力バイト列を
    // 一切変えないためfalseを渡す。GUIはExcelでの見た目を優先してtrueを渡す想定。
    write_bom: bool,
    // trueのとき、全ての値をダブルクォートで囲んで書き出す(CSVの標準的な
    // クォート規則により、値の中の"は""にエスケープされる)。名称にスペースを
    // 含むケースなど、値の区切りを明確にしたい場合にオンにする用途を想定している。
    // falseのとき(既定)は今まで通り、カンマ・改行・"を含む値だけを囲む
    // (csvクレートのQuoteStyle::Necessaryのデフォルト挙動)。
    quote_all: bool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<(), Box<dyn std::error::Error>> {
    // File::create(path)は指定したパスに新しいファイルを作る(既にあれば中身を空にする)。
    // BufWriterは「書き込みをまとめて行う」ラッパーで、1バイトずつファイルに直接書き込むより
    // 速くなる(内部にバッファ=一時的な貯め場所を持ち、ある程度たまってからまとめて書き出す)
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
    // write_all(&[0xEF, 0xBB, 0xBF])は、UTF-8のBOMを表す3バイトをそのままファイルの
    // 先頭に書き込む処理(&[...]はバイトの配列への参照)
    if write_bom && matches!(encoding, Encoding::Utf8) {
        out.write_all(&[0xEF, 0xBB, 0xBF])?;
    }
    let mut had_sjis_errors = false;
    let total = row_count as u64;
    let mut done: u64 = 0;
    let quote_style = if quote_all { csv::QuoteStyle::Always } else { csv::QuoteStyle::Necessary };

    if row_count == 0 {
        // row_countが0でも、build_csv_from_rowsと同様にヘッダー行だけは書き出す
        let mut writer = csv::WriterBuilder::new().quote_style(quote_style).from_writer(Vec::new());
        let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        writer.write_record(&headers)?;
        let text = String::from_utf8(writer.into_inner()?)?;
        write_chunk_text(&mut out, &text, encoding, &mut had_sjis_errors)?;
        on_progress(0, 0);
    }

    // (1..=row_count).step_by(chunk_size)は「1からrow_countまでを、chunk_size個おきに
    // 取り出す」という範囲の作り方。例えばrow_count=10000, chunk_size=1000なら、
    // chunk_startは1, 1001, 2001, ... と1000おきの値になり、1回のループで1000行ずつ処理する
    for chunk_start in (1..=row_count).step_by(chunk_size as usize) {
        // このチャンク(かたまり)の終わりの行番号。chunk_start+chunk_size-1が本来の終わりだが、
        // それがrow_countを超える場合(最後のチャンクで端数が出る場合)はrow_countで打ち切る。
        // .min(row_count)は「2つの値のうち小さい方を選ぶ」メソッド
        let chunk_end = (chunk_start + chunk_size - 1).min(row_count);
        let rows = generate_rows_range(columns, base_seed, chunk_start, chunk_end);

        // このチャンク分だけのCSVテキストを組み立てる(build_csv_from_rowsと同じ要領だが、
        // ヘッダー行は最初のチャンク(chunk_start == 1)のときだけ書く)
        let mut writer = csv::WriterBuilder::new().quote_style(quote_style).from_writer(Vec::new());
        if chunk_start == 1 {
            let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
            writer.write_record(&headers)?;
        }
        for row in &rows {
            let record: Vec<&str> = row.iter().map(|cell| cell.as_deref().unwrap_or("")).collect();
            writer.write_record(&record)?;
        }
        let text = String::from_utf8(writer.into_inner()?)?;
        // 組み立てたテキストを、指定の文字コードに変換してファイルに書き足す
        // (rows自体はこの後使われないので、次のループでメモリから解放される=溜め込まれない)
        write_chunk_text(&mut out, &text, encoding, &mut had_sjis_errors)?;

        done += (chunk_end - chunk_start + 1) as u64;
        on_progress(done, total);
    }

    // flush()は「BufWriterの内部バッファに残っている分を、確実にファイルへ書き出す」処理。
    // BufWriterは自動でも書き出すが、関数の最後で明示的に呼んで書き漏れが無いようにしている
    out.flush()?;
    if had_sjis_errors {
        eprintln!("警告: Shift-JISに変換できない文字が '?' に置き換えられました");
    }
    Ok(())
}

// UTF-8のテキストを、指定された文字コードでファイルに書き出す(CSV/SQL共通の後処理)
// UTF-8の文字列をShift-JISのバイト列に変換する。
// encoding_rsの encode() / encode_from_utf8() は(HTMLの仕様に合わせて)変換できない文字を
// "&#12345;" のようなHTML文字参照に置き換えるが、CSV/SQLのデータにHTML文字参照が紛れ込むと
// (取り込み先ではHTMLとして解釈されないので)ただの意味不明な文字列になってしまう。
// そのため、変換できない文字を自分で検出して "?" 1文字に置き換える
// (encode_from_utf8_without_replacement を使う、encoding_rs公式ドキュメント記載の定石)。
fn encode_to_sjis(text: &str) -> (Vec<u8>, bool) {
    let mut encoder = encoding_rs::SHIFT_JIS.new_encoder();
    let mut out = Vec::with_capacity(text.len());
    let mut buf = [0u8; 4096];
    let mut remaining = text;
    let mut had_errors = false;

    loop {
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(remaining, &mut buf, true);
        out.extend_from_slice(&buf[..written]);
        remaining = &remaining[read..];

        match result {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::OutputFull => continue, // bufが埋まっただけ。続きを処理する
            encoding_rs::EncoderResult::Unmappable(_) => {
                out.push(b'?');
                had_errors = true;
            }
        }
    }

    (out, had_errors)
}

pub fn write_text(text: &str, path: &str, encoding: Encoding) -> Result<(), Box<dyn std::error::Error>> {
    match encoding {
        Encoding::Utf8 => std::fs::write(path, text)?,
        Encoding::Sjis => {
            let (bytes, had_errors) = encode_to_sjis(text);
            if had_errors {
                eprintln!("警告: Shift-JISに変換できない文字が '?' に置き換えられました");
            }
            std::fs::write(path, bytes)?;
        }
    }

    Ok(())
}

// 列タイプに応じて、文字列の値を適切なJSONの型(数値・真偽値・文字列・null)に変換する
fn cell_to_json(kind: &PreparedColumnType, cell: Option<&str>) -> serde_json::Value {
    let value = match cell {
        Some(v) => v,
        None => return serde_json::Value::Null,
    };

    match kind {
        PreparedColumnType::Sequence => value.parse::<u64>().map(Into::into).unwrap_or(serde_json::Value::Null),
        PreparedColumnType::Integer { .. } => {
            value.parse::<i64>().map(Into::into).unwrap_or(serde_json::Value::Null)
        }
        PreparedColumnType::Float { .. } => value
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        PreparedColumnType::Boolean => serde_json::Value::Bool(value == "true"),
        PreparedColumnType::ForeignKey { repr, .. } => match repr {
            FkRepr::Integer => value.parse::<i64>().map(Into::into).unwrap_or(serde_json::Value::Null),
            FkRepr::Float => value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            FkRepr::Boolean => serde_json::Value::Bool(value == "true"),
            FkRepr::Text => serde_json::Value::String(value.to_string()),
        },
        _ => serde_json::Value::String(value.to_string()),
    }
}

// JSON(NDJSON = 1行1件のJSONオブジェクト)の中身(行データは生成済みのものを受け取る)を
// UTF-8の文字列として組み立てる
pub fn build_json_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
) -> Result<String, Box<dyn std::error::Error>> {
    // 1行につき1つのJSONオブジェクト(文字列)を作り、linesという一覧にまとめる
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            // serde_json::Mapは「キーと値の組」を順序を保ったまま持てる入れ物(JSONの{}に対応する)。
            // with_capacity(columns.len())は、最終的に列の数だけ入ることが分かっているので、
            // 内部の領域を先に確保しておく最適化(無くても動作は変わらない)
            let mut object = serde_json::Map::with_capacity(columns.len());
            for (column, cell) in columns.iter().zip(row) {
                object.insert(column.name.clone(), cell_to_json(&column.kind, cell.as_deref()));
            }
            // serde_json::to_string(...)はMapをJSON形式の文字列に変換するメソッド。
            // expect(...)は「ここで失敗するとしたらプログラムのバグなので、その場で
            // 止めて知らせる」という意図(通常のString/数値/真偽値だけを詰めているMapが
            // JSON化に失敗することは無い)
            serde_json::to_string(&object).expect("serde_jsonのオブジェクト直列化は失敗しない")
        })
        .collect();

    let mut text = lines.join("\n");
    text.push('\n'); // CSV/SQL出力と同様、ファイル末尾に改行を入れておく
    Ok(text)
}

// build_json_from_rowsの「行数とシードを渡すだけで一発で作れる」版(テストからのみ使用)
#[cfg(test)]
fn build_json(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    build_json_from_rows(columns, &generate_all_rows(row_count, columns, base_seed))
}

// 列タイプに応じて、Excelのセルに数値/真偽値/文字列として書き込む。NULL(None)は何も
// 書かない(Excel上は空白セルになる。これが最も自然なNULLの表現)
fn write_xlsx_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    kind: &PreparedColumnType,
    cell: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // セルの値がNone(NULL)なら、何も書き込まずにここで終わる(Excel上は空白セルになる)
    let Some(value) = cell else {
        return Ok(());
    };

    match kind {
        PreparedColumnType::Sequence | PreparedColumnType::Integer { .. } | PreparedColumnType::Float { .. } => {
            // value.parse::<f64>()は「文字列を小数として読み取れるか試す」処理で、
            // 成功すればOk(数値)、失敗すればErrになる。"if let Ok(number) = ... "は
            // 「読み取れたときだけ」中に入る構文で、読み取れれば数値セルとして、
            // 読み取れなければ(想定外の値が来た場合の保険として)文字列セルとして書き込む
            if let Ok(number) = value.parse::<f64>() {
                worksheet.write_number(row, col, number)?;
            } else {
                worksheet.write_string(row, col, value)?;
            }
        }
        PreparedColumnType::Boolean => {
            worksheet.write_boolean(row, col, value == "true")?;
        }
        PreparedColumnType::ForeignKey { repr, .. } => match repr {
            FkRepr::Integer | FkRepr::Float => {
                if let Ok(number) = value.parse::<f64>() {
                    worksheet.write_number(row, col, number)?;
                } else {
                    worksheet.write_string(row, col, value)?;
                }
            }
            FkRepr::Boolean => {
                worksheet.write_boolean(row, col, value == "true")?;
            }
            FkRepr::Text => {
                worksheet.write_string(row, col, value)?;
            }
        },
        _ => {
            worksheet.write_string(row, col, value)?;
        }
    }

    Ok(())
}

// Excel(.xlsx)ファイルを直接組み立てて保存する(行データは生成済みのものを受け取る)。
// xlsxはCSV/SQL/JSONと違ってテキストではなくバイナリ(実体はZIP)形式なので、
// build_*_from_rowsのように文字列を返してwrite_textに渡す、という流れには乗せられず、
// ここだけ保存まで独立して行っている。また、xlsxは常にUTF-8相当の内部表現を持つ
// ファイル形式のため、--encodingは効かない。
pub fn write_xlsx_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let table = GeneratedTable { name: None, columns, rows };
    // use_sheet_names=falseにして、rust_xlsxwriterの既定のシート名(Sheet1)のままにする
    // (複数テーブル出力とブックの中身を分けるため)
    write_xlsx_tables(&[table], path, false)
}

// write_xlsx_from_rowsの「行数とシードを渡すだけで一発で作れる」版(テストからのみ使用)
#[cfg(test)]
fn write_xlsx(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    write_xlsx_from_rows(columns, &generate_all_rows(row_count, columns, base_seed), path)
}

fn format_extension(format: Format) -> &'static str {
    match format {
        Format::Csv => "csv",
        Format::Sql => "sql",
        Format::Json => "json",
        Format::Xlsx => "xlsx",
    }
}

fn default_output_path(format: Format) -> String {
    format!("output.{}", format_extension(format))
}

// --outputと形式から、実際に書き込むパスを決める。
// 形式が1つだけのときは今まで通りの挙動(--outputを文字通り使う。省略時はoutput.{ext})。
// 形式が複数のときは--outputを「拡張子なしのベース名」として扱い、
// 形式ごとに拡張子を付ける(例: "result"+csv → "result.csv")。
// 拡張子付きで指定された場合(例: "result.csv")は、末尾の拡張子を1つ取り除いてベース名にする。
pub fn output_base_path(output: Option<&str>, format: Format, multiple_formats: bool) -> String {
    if !multiple_formats {
        return output.map(str::to_string).unwrap_or_else(|| default_output_path(format));
    }

    let stem = match output {
        Some(o) => match o.rsplit_once('.') {
            Some((stem, _ext)) if !stem.is_empty() => stem.to_string(),
            _ => o.to_string(),
        },
        None => "output".to_string(),
    };
    format!("{}.{}", stem, format_extension(format))
}

// 指定された1つの形式について、既に生成済みの行データ(rows)を清書してファイルに保存する。
// rowsを引数で受け取ることで、複数形式を同時出力しても値の生成(generate_all_rows)は
// 1回で済む(形式ごとに毎回同じ乱数列から生成し直すのは無駄なため)。単一テーブル専用。
pub fn write_output(
    format: Format,
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    table_name: Option<&str>,
    path: &str,
    encoding: Encoding,
) -> Result<(), Box<dyn std::error::Error>> {
    match format {
        // 複数形式同時出力の経路であり、CLIの--quote-allは(ドキュメント通り)ここには適用しない
        Format::Csv => write_text(&build_csv_from_rows(columns, rows, false)?, path, encoding),
        Format::Sql => {
            let table_name = match table_name {
                Some(t) => t,
                None => {
                    return Err(
                        "SQL出力(--format sql)には、schema.yamlに table_name の指定が必要です".into()
                    );
                }
            };
            write_text(&build_sql_from_rows(columns, rows, table_name)?, path, encoding)
        }
        Format::Json => write_text(&build_json_from_rows(columns, rows)?, path, encoding),
        Format::Xlsx => write_xlsx_from_rows(columns, rows, path),
    }
}

// 生成済みの1テーブル分(複数テーブル出力で使う)。columns/rowsは借用のみで、
// 実体はmain()側のtables/rows_by_tableが持ち続ける
pub struct GeneratedTable<'a> {
    pub name: Option<&'a str>,
    pub columns: &'a [PreparedColumn],
    pub rows: &'a [Vec<Option<String>>],
}

// ファイル名に使えない文字( \ / : * ? " < > | と制御文字)を "_" に置き換える
fn sanitize_file_component(name: &str) -> String {
    name.chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c })
        .collect()
}

// 複数テーブルでcsv/jsonを出すときの、テーブルごとのファイル名。
// "result.csv" + "users" → "result_users.csv"(拡張子の直前にテーブル名を差し込む)
fn table_file_path(base: &str, table_name: &str) -> String {
    let sanitized = sanitize_file_component(table_name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{}_{}.{}", stem, sanitized, ext),
        _ => format!("{}_{}", base, sanitized),
    }
}

// Excelのシート名の制約(31文字以内、[ ] : * ? / \ 不可、重複不可)に合わせて名前を整形する
fn sanitize_sheet_name(name: &str, used: &mut std::collections::HashSet<String>) -> String {
    let cleaned: String = name.chars().map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).collect();
    let cleaned: String = cleaned.chars().take(31).collect();
    let cleaned = if cleaned.is_empty() { "Sheet".to_string() } else { cleaned };

    if !used.contains(&cleaned) {
        used.insert(cleaned.clone());
        return cleaned;
    }

    let mut suffix = 2;
    loop {
        let marker = format!("_{}", suffix);
        let keep = 31usize.saturating_sub(marker.len());
        let candidate = format!("{}{}", cleaned.chars().take(keep).collect::<String>(), marker);
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        suffix += 1;
    }
}

// 複数テーブルを依存順(親が先)に受け取り、1本のSQLにまとめる。
// 外部キー制約のあるDBにそのまま流し込めるよう、親のテーブルのINSERT文を先に出力する
fn build_sql_multi(tables: &[GeneratedTable]) -> Result<String, Box<dyn std::error::Error>> {
    let mut sql = String::new();
    for table in tables {
        let table_name = table.name.expect("複数テーブル形式ではtable_nameが必須(normalize_schema_fileで保証済み)");
        sql.push_str(&format!("-- テーブル: {}\n", table_name));
        sql.push_str(&build_sql_from_rows(table.columns, table.rows, table_name)?);
        sql.push('\n');
    }
    Ok(sql)
}

// 複数テーブルを1つのExcelブックにまとめる(テーブルごとに1シート)。
// use_sheet_namesがfalseのときはシート名を設定せず、rust_xlsxwriterの既定(Sheet1等)のままにする
// (単一テーブルのwrite_xlsx_from_rowsと完全に同じブックになるようにするため)
fn write_xlsx_tables(
    tables: &[GeneratedTable],
    path: &str,
    use_sheet_names: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let mut used_sheet_names = std::collections::HashSet::new();

    for table in tables {
        let worksheet = workbook.add_worksheet();
        if use_sheet_names {
            let name = table.name.unwrap_or("Sheet");
            worksheet.set_name(sanitize_sheet_name(name, &mut used_sheet_names))?;
        }

        for (col_idx, column) in table.columns.iter().enumerate() {
            worksheet.write_string(0, col_idx as u16, &column.name)?;
        }

        for (row_idx, row) in table.rows.iter().enumerate() {
            let excel_row = (row_idx + 1) as u32;
            for (col_idx, (column, cell)) in table.columns.iter().zip(row).enumerate() {
                write_xlsx_cell(worksheet, excel_row, col_idx as u16, &column.kind, cell.as_deref())?;
            }
        }
    }

    workbook.save(path)?;
    Ok(())
}

// 複数テーブルのとき、指定した形式について全テーブル分を書き出す。
// 戻り値は書き込んだ(パス, 行数)の一覧(mainが成功メッセージを1行ずつ表示するために使う)。
// quote_all: Csv形式のときだけ使う(単一テーブルのwrite_csv_streamingと同じ意味。
// sql/json/xlsxには影響しない)
pub fn write_output_multi_table(
    format: Format,
    tables: &[GeneratedTable],
    base_path: &str,
    encoding: Encoding,
    quote_all: bool,
) -> Result<Vec<(String, u32)>, Box<dyn std::error::Error>> {
    match format {
        // csv/jsonはテーブルごとに別ファイルに保存する形式なので、同じ処理でまとめて扱う
        Format::Csv | Format::Json => {
            // 書き込みを始める前に、サニタイズ後のファイル名が衝突しないか確認する。
            // 先に全テーブル分のパスを計算してpathsに集め、containsで「もう同じパスが
            // 無いか」を1つずつ確認する(1つでも衝突があれば、途中まで書き込んでしまう前に
            // エラーで止められる)
            let mut paths = Vec::with_capacity(tables.len());
            for table in tables {
                let path = table_file_path(base_path, table.name.unwrap_or(""));
                if paths.contains(&path) {
                    return Err(format!(
                        "出力ファイル名 \"{}\" が衝突します。テーブル名を変えてください",
                        path
                    )
                    .into());
                }
                paths.push(path);
            }

            let mut written = Vec::with_capacity(tables.len());
            // zip(...)は「2つの一覧を先頭同士、2番目同士…と1対1でペアにする」メソッド。
            // ここではtables(テーブルの中身)とpaths(保存先パス)を組にして、1テーブルずつ処理する
            for (table, path) in tables.iter().zip(paths) {
                let text = match format {
                    Format::Csv => build_csv_from_rows(table.columns, table.rows, quote_all)?,
                    Format::Json => build_json_from_rows(table.columns, table.rows)?,
                    _ => unreachable!("csv/json以外はこの分岐に来ない"),
                };
                write_text(&text, &path, encoding)?;
                written.push((path, table.rows.len() as u32));
            }
            Ok(written)
        }
        Format::Sql => {
            write_text(&build_sql_multi(tables)?, base_path, encoding)?;
            let total_rows: u32 = tables.iter().map(|t| t.rows.len() as u32).sum();
            Ok(vec![(base_path.to_string(), total_rows)])
        }
        Format::Xlsx => {
            write_xlsx_tables(tables, base_path, true)?;
            let total_rows: u32 = tables.iter().map(|t| t.rows.len() as u32).sum();
            Ok(vec![(base_path.to_string(), total_rows)])
        }
    }
}

// `cargo test` で実行されるテスト。#[cfg(test)] が付いた部分は通常のビルドには含まれない
#[cfg(test)]
mod tests {
    use super::*;

    fn schema_from_yaml(yaml: &str) -> Schema {
        serde_yaml::from_str(yaml).expect("テスト用YAMLのパースに失敗した")
    }

    #[test]
    fn schema_file_to_yaml_single_table_round_trips_through_load_schema() {
        let schema = schema_from_yaml(
            "row_count: 3\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n  - name: email\n    type: email\n    domain: test.example\n  - name: age\n    type: integer\n    min: 18\n    max: 65\n    unique: true\n",
        );
        let file = SchemaFile { tables: vec![schema], multi_table: false };

        let yaml = schema_file_to_yaml(&file).unwrap();
        // 単一テーブルは"tables:"では包まず、今まで通りトップレベルに直接書く見た目になる
        assert!(!yaml.contains("tables:"));

        let reloaded: RawSchemaFile = serde_yaml::from_str(&yaml).unwrap();
        let reloaded = normalize_schema_file(reloaded).unwrap();
        assert!(!reloaded.multi_table);
        assert_eq!(reloaded.tables.len(), 1);
        assert_eq!(reloaded.tables[0].row_count, 3);
        assert_eq!(reloaded.tables[0].table_name.as_deref(), Some("users"));
        assert_eq!(reloaded.tables[0].columns.len(), 3);
    }

    #[test]
    fn schema_file_to_yaml_multi_table_round_trips_through_load_schema() {
        let users = schema_from_yaml("row_count: 5\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n");
        let orders = schema_from_yaml(
            "row_count: 8\ntable_name: orders\ncolumns:\n  - name: id\n    type: sequence\n  - name: user_id\n    type: foreign_key\n    references: users.id\n",
        );
        let file = SchemaFile { tables: vec![users, orders], multi_table: true };

        let yaml = schema_file_to_yaml(&file).unwrap();
        assert!(yaml.contains("tables:"));

        let reloaded: RawSchemaFile = serde_yaml::from_str(&yaml).unwrap();
        let reloaded = normalize_schema_file(reloaded).unwrap();
        assert!(reloaded.multi_table);
        assert_eq!(reloaded.tables.len(), 2);
        assert_eq!(reloaded.tables[0].table_name.as_deref(), Some("users"));
        assert_eq!(reloaded.tables[1].table_name.as_deref(), Some("orders"));
    }

    // 回帰テスト: 以前はtables:形式のテーブル名が空白だけ(例: "   ")でも
    // 「テーブル名が指定されていない」扱いにならず、そのまま通ってしまっていた
    #[test]
    fn normalize_schema_file_rejects_whitespace_only_table_name() {
        let raw: RawSchemaFile = serde_yaml::from_str(
            "tables:\n  - name: \"   \"\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n",
        )
        .unwrap();
        assert!(normalize_schema_file(raw).is_err());
    }

    // 回帰テスト: dummygen_jp_gui(GUI)から複数テーブルを直接生成・プレビューする経路は
    // SchemaFileをnormalize_schema_file(schema.yaml読み込み時の検証)を経由せず自分で
    // 組み立てるため、以前はテーブル名が重複していても・空でもprepare_tablesを素通りしてしまい、
    // SQL出力(build_sql_multi)で2つの別テーブルが同じテーブル名のINSERT文に混ざったり、
    // 外部キーの参照先(resolve_foreign_keysのname_to_index)が意図しない方のテーブルに
    // すり替わったりしていた
    #[test]
    fn prepare_tables_rejects_duplicate_table_names_without_going_through_yaml() {
        let users_a = schema_from_yaml("row_count: 5\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n");
        let users_b = schema_from_yaml(
            "row_count: 5\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n  - name: score\n    type: integer\n    min: 0\n    max: 100\n",
        );
        let file = SchemaFile { tables: vec![users_a, users_b], multi_table: true };

        // PreparedTableはDebugを持たないため、unwrap_err()ではなくerr().unwrap()を使う
        let err = prepare_tables(&file).err().unwrap();
        assert!(err.to_string().contains("重複しています"), "エラーメッセージ: {}", err);
    }

    #[test]
    fn prepare_tables_rejects_blank_table_name_in_multi_table_mode() {
        let file = SchemaFile {
            tables: vec![schema_from_yaml(
                "row_count: 5\ntable_name: \"   \"\ncolumns:\n  - name: id\n    type: sequence\n",
            )],
            multi_table: true,
        };

        assert!(prepare_tables(&file).is_err());
    }

    // 単一テーブル形式(multi_table: false)ではtable_name未指定が正当な使い方
    // (CSV/Excel出力等)なので、この検証で誤ってエラーにしないことを確認する
    #[test]
    fn prepare_tables_allows_missing_table_name_in_single_table_mode() {
        let file = SchemaFile {
            tables: vec![schema_from_yaml("row_count: 3\ncolumns:\n  - name: id\n    type: sequence\n")],
            multi_table: false,
        };

        assert!(prepare_tables(&file).is_ok());
    }

    // 複数テーブル(tables:形式)のCSV出力でも、quote_all: trueで全ての値がダブルクォートで
    // 囲まれることを確認する(以前はwrite_output_multi_tableにこの引数が無く、常にfalse相当だった)
    #[test]
    fn write_output_multi_table_csv_with_quote_all_true_quotes_every_field() {
        let users_schema = schema_from_yaml(
            "row_count: 1\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: fixed\n    value: \"山田 太郎\"\n",
        );
        let users_columns = prepare_columns(&users_schema).unwrap();
        let users_rows = generate_all_rows(users_schema.row_count, &users_columns, 1);
        let users_table = GeneratedTable { name: Some("users"), columns: &users_columns, rows: &users_rows };

        let dir = std::env::temp_dir();
        let base_path = dir.join("dummy_data_gen_test_multi_quote_all.csv");
        let base_path_str = base_path.to_str().unwrap();

        write_output_multi_table(Format::Csv, &[users_table], base_path_str, Encoding::Utf8, true).unwrap();

        let written_path = table_file_path(base_path_str, "users");
        let actual = std::fs::read_to_string(&written_path).unwrap();
        assert_eq!(actual, "\"id\",\"name\"\n\"1\",\"山田 太郎\"\n");

        let _ = std::fs::remove_file(&written_path);
    }

    // quote_all: false(既定)のときは、複数テーブルのCSV出力でも今まで通り
    // 必要な値だけがクォートされることを確認する
    #[test]
    fn write_output_multi_table_csv_with_quote_all_false_only_quotes_when_necessary() {
        let users_schema = schema_from_yaml(
            "row_count: 1\ntable_name: users\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: fixed\n    value: \"山田 太郎\"\n",
        );
        let users_columns = prepare_columns(&users_schema).unwrap();
        let users_rows = generate_all_rows(users_schema.row_count, &users_columns, 1);
        let users_table = GeneratedTable { name: Some("users"), columns: &users_columns, rows: &users_rows };

        let dir = std::env::temp_dir();
        let base_path = dir.join("dummy_data_gen_test_multi_quote_none.csv");
        let base_path_str = base_path.to_str().unwrap();

        write_output_multi_table(Format::Csv, &[users_table], base_path_str, Encoding::Utf8, false).unwrap();

        let written_path = table_file_path(base_path_str, "users");
        let actual = std::fs::read_to_string(&written_path).unwrap();
        assert_eq!(actual, "id,name\n1,山田 太郎\n");

        let _ = std::fs::remove_file(&written_path);
    }

    #[test]
    fn enum_with_heavily_skewed_weights_almost_always_picks_the_heavy_choice() {
        let schema = schema_from_yaml(
            "row_count: 1000\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n    weights: [100.0, 0.0]\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(schema.row_count, &columns, 42);

        for row in &rows {
            assert_eq!(row[0].as_deref(), Some("A"));
        }
    }

    #[test]
    fn enum_without_weights_still_picks_uniformly_at_random() {
        // weights省略時、今まで通り両方の選択肢が出ること(後方互換の確認)
        let schema = schema_from_yaml(
            "row_count: 200\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(schema.row_count, &columns, 42);

        let has_a = rows.iter().any(|r| r[0].as_deref() == Some("A"));
        let has_b = rows.iter().any(|r| r[0].as_deref() == Some("B"));
        assert!(has_a && has_b);
    }

    #[test]
    fn enum_weights_length_mismatch_is_rejected() {
        let schema = schema_from_yaml(
            "row_count: 1\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n    weights: [1.0]\n",
        );
        let err = prepare_columns(&schema).err().unwrap();
        assert!(err.to_string().contains("weights"));
    }

    #[test]
    fn enum_negative_weight_is_rejected() {
        let schema = schema_from_yaml(
            "row_count: 1\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n    weights: [1.0, -1.0]\n",
        );
        let err = prepare_columns(&schema).err().unwrap();
        assert!(err.to_string().contains("負の数"));
    }

    #[test]
    fn enum_all_zero_weights_is_rejected() {
        let schema = schema_from_yaml(
            "row_count: 1\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n    weights: [0.0, 0.0]\n",
        );
        let err = prepare_columns(&schema).err().unwrap();
        assert!(err.to_string().contains("合計"));
    }

    #[test]
    fn enum_unique_ignores_weights_and_still_enumerates_all_choices_exactly_once() {
        let schema = schema_from_yaml(
            "row_count: 2\ntable_name: t\ncolumns:\n  - name: status\n    type: enum\n    choices: [\"A\", \"B\"]\n    weights: [100.0, 1.0]\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 42);
        let rows = generate_all_rows(schema.row_count, &columns, 42);

        let mut values: Vec<&str> = rows.iter().map(|r| r[0].as_deref().unwrap()).collect();
        values.sort();
        assert_eq!(values, vec!["A", "B"]);
    }

    #[test]
    fn sql_literal_quotes_text_columns_and_escapes_quote() {
        let quoted = sql_literal(&PreparedColumnType::NameJa { with_space: false }, "O'Brien");
        assert_eq!(quoted, "'O''Brien'");
    }

    // 回帰テスト: 列名にカンマが含まれると、以前はINSERT文の列数とVALUESの値の数が
    // ずれて壊れたSQLになっていた(sql_ident で列名/テーブル名をクォートして修正)
    #[test]
    fn build_sql_quotes_column_names_containing_comma() {
        let schema = schema_from_yaml(
            "row_count: 1\ntable_name: t\ncolumns:\n  - name: \"id, name\"\n    type: sequence\n  - name: email\n    type: email\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let sql = build_sql(schema.row_count, &columns, "t", 42).unwrap();
        assert!(sql.contains("\"id, name\""));
        assert!(sql.starts_with("INSERT INTO \"t\" (\"id, name\", \"email\") VALUES"));
        // 値は2個(列も2個)であるべき
        let values_line = sql.lines().nth(1).unwrap();
        assert_eq!(values_line.matches(',').count(), 1);
    }

    #[test]
    fn prepare_columns_rejects_empty_columns() {
        let schema = schema_from_yaml("row_count: 1\ncolumns: []\n");
        assert!(prepare_columns(&schema).is_err());
    }

    // 回帰テスト: 以前はrow_count: 0でもエラーにならず、ヘッダ行だけの0行ファイルが
    // 何も警告なく生成できてしまっていた
    #[test]
    fn prepare_columns_rejects_row_count_zero() {
        let schema = schema_from_yaml("row_count: 0\ncolumns:\n  - name: id\n    type: sequence\n");
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_duplicate_column_names() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n  - name: id\n    type: email\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    // 回帰テスト: 以前は列名が空文字・空白だけでもエラーにならず、SQL出力すると
    // INSERT INTO "t" ("") VALUES ... のような、実際のデータベースでは無効な
    // 識別子としてエラーになりうるSQLが生成できてしまっていた
    #[test]
    fn prepare_columns_rejects_blank_column_name() {
        let empty = schema_from_yaml("row_count: 1\ncolumns:\n  - name: \"\"\n    type: sequence\n");
        assert!(prepare_columns(&empty).is_err());
        let whitespace_only =
            schema_from_yaml("row_count: 1\ncolumns:\n  - name: \"   \"\n    type: sequence\n");
        assert!(prepare_columns(&whitespace_only).is_err());
    }

    // 回帰テスト: 以前はtable_nameが空白だけ(例: "   ")でもSQL出力時にエラーにならず、
    // INSERT INTO "   " (...) のような実用上意味のないSQLが生成できてしまっていた
    #[test]
    fn build_sql_from_rows_rejects_blank_table_name() {
        let schema = schema_from_yaml("row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n");
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(1, &columns, 1);
        assert!(build_sql_from_rows(&columns, &rows, "").is_err());
        assert!(build_sql_from_rows(&columns, &rows, "   ").is_err());
    }

    // 回帰テスト: 以前はShift-JISで表現できない文字をHTML文字参照("&#12345;")に
    // 置き換えていたため、CSV/SQLの中に意味不明な文字列が紛れ込んでいた
    #[test]
    fn encode_to_sjis_replaces_unmappable_chars_with_question_mark_not_html_entity() {
        let (bytes, had_errors) = encode_to_sjis("絵文字🎉列");
        assert!(had_errors);
        let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(&bytes);
        assert_eq!(decoded, "絵文字?列");
    }

    #[test]
    fn sql_literal_does_not_quote_numeric_or_boolean_columns() {
        assert_eq!(sql_literal(&PreparedColumnType::Integer { min: 0, max: 10 }, "5"), "5");
        assert_eq!(sql_literal(&PreparedColumnType::Boolean, "true"), "true");
    }

    #[test]
    fn prepare_columns_rejects_integer_min_greater_than_max() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: age\n    type: integer\n    min: 65\n    max: 18\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_invalid_date_format() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: d\n    type: date\n    start: not-a-date\n    end: \"2020-01-01\"\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_start_after_end() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: d\n    type: date\n    start: \"2020-01-01\"\n    end: \"2019-01-01\"\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_null_rate_out_of_range() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: n\n    type: email\n    null_rate: 1.5\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_accepts_valid_schema() {
        let schema = schema_from_yaml(
            "row_count: 3\ncolumns:\n  - name: id\n    type: sequence\n  - name: age\n    type: integer\n    min: 18\n    max: 65\n",
        );
        let columns = prepare_columns(&schema).expect("妥当なスキーマはエラーにならないはず");
        assert_eq!(columns.len(), 2);
    }

    #[test]
    fn random_department_is_in_dictionary() {
        let mut rng = row_rng(1, 1);
        for _ in 0..50 {
            assert!(DEPARTMENTS.contains(&random_department(&mut rng).as_str()));
        }
    }

    #[test]
    fn random_job_title_is_in_dictionary() {
        let mut rng = row_rng(1, 1);
        for _ in 0..50 {
            assert!(JOB_TITLES.contains(&random_job_title(&mut rng).as_str()));
        }
    }

    #[test]
    fn random_ip_address_has_valid_format() {
        let mut rng = row_rng(1, 1);
        for _ in 0..50 {
            let value = random_ip_address(&mut rng);
            let octets: Vec<&str> = value.split('.').collect();
            assert_eq!(octets.len(), 4);
            for octet in octets {
                let n: u32 = octet.parse().expect("各オクテットは数値のはず");
                assert!(n <= 255);
            }
        }
    }

    #[test]
    fn random_jwt_has_three_dot_separated_segments() {
        let mut rng = row_rng(1, 1);
        let value = random_jwt(&mut rng);
        let segments: Vec<&str> = value.split('.').collect();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], JWT_HEADER);
    }

    #[test]
    fn random_api_key_has_sk_prefix_and_expected_length() {
        let mut rng = row_rng(1, 1);
        let value = random_api_key(&mut rng);
        assert!(value.starts_with("sk_"));
        assert_eq!(value.len(), 35); // "sk_" (3文字) + 英数字32文字
    }

    #[test]
    fn random_username_is_lowercase_name_plus_number() {
        let mut rng = row_rng(1, 1);
        let value = random_username(&mut rng);
        assert!(value.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert!(value.chars().any(|c| c.is_ascii_digit()));
    }

    #[test]
    fn random_password_has_expected_length() {
        let mut rng = row_rng(1, 1);
        let value = random_password(&mut rng);
        assert_eq!(value.chars().count(), 12);
    }

    #[test]
    fn random_profile_image_url_has_placehold_jp_format() {
        let mut rng = row_rng(1, 1);
        let value = random_profile_image_url(&mut rng);
        assert!(value.starts_with("https://placehold.jp/"));
        assert!(value.contains("x"));
        assert!(value.contains(".png?id="));
    }

    #[test]
    fn luhn_check_digit_matches_known_example() {
        // "4111111111111111"(有名なVisaテストカード番号、16桁、Luhn検査済み)の
        // 先頭15桁から検査数字(16桁目)を再計算し、実際の16桁目と一致するか確認する
        let digits: Vec<u8> = "4111111111111111".chars().map(|c| c.to_digit(10).unwrap() as u8).collect();
        assert_eq!(digits.len(), 16);
        let (body, expected_check) = digits.split_at(15);
        assert_eq!(luhn_check_digit(body), expected_check[0]);
    }

    #[test]
    fn random_credit_card_number_passes_luhn_check() {
        let mut rng = row_rng(1, 1);
        for _ in 0..50 {
            let value = random_credit_card_number(&mut rng);
            assert_eq!(value.len(), 16);
            let digits: Vec<u8> = value.chars().map(|c| c.to_digit(10).unwrap() as u8).collect();
            let (body, check) = digits.split_at(15);
            assert_eq!(luhn_check_digit(body), check[0]);
        }
    }

    #[test]
    fn my_number_check_digit_matches_hand_calculation() {
        // 手計算での検算: digits(左から) = [1,2,3,4,5,6,7,8,9,0,1]
        // 右から1始まりのi=1..11、weight(i)= i<=6 ? i+1 : i-5
        // i:  11 10  9  8  7  6  5  4  3  2  1
        // w:   6  5  4  3  2  7  6  5  4  3  2
        // d:   1  2  3  4  5  6  7  8  9  0  1 (右からi=1が末尾桁)
        // 対応する桁(左から): digits[0]=1(i=11,w=6) ... digits[10]=1(i=1,w=2)
        let digits: [u8; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1];
        let weights = [6, 5, 4, 3, 2, 7, 6, 5, 4, 3, 2];
        let sum: u32 = digits.iter().zip(weights.iter()).map(|(&d, &w)| d as u32 * w as u32).sum();
        let remainder = sum % 11;
        let expected = if remainder <= 1 { 0 } else { (11 - remainder) as u8 };
        assert_eq!(my_number_check_digit(&digits), expected);
    }

    #[test]
    fn random_my_number_check_digit_is_internally_consistent() {
        let mut rng = row_rng(1, 1);
        for _ in 0..50 {
            let value = random_my_number(&mut rng);
            assert_eq!(value.len(), 12);
            let digits: Vec<u8> = value.chars().map(|c| c.to_digit(10).unwrap() as u8).collect();
            let body: [u8; 11] = digits[0..11].try_into().unwrap();
            assert_eq!(my_number_check_digit(&body), digits[11]);
        }
    }

    #[test]
    fn generate_value_integer_stays_within_range() {
        let mut rng = row_rng(42, 1);
        for _ in 0..200 {
            let kind = PreparedColumnType::Integer { min: 18, max: 65 };
            let value: i64 = generate_value(&kind, 1, &mut rng).parse().unwrap();
            assert!((18..=65).contains(&value));
        }
    }

    #[test]
    fn generate_value_boolean_is_true_or_false() {
        let mut rng = row_rng(42, 1);
        let value = generate_value(&PreparedColumnType::Boolean, 1, &mut rng);
        assert!(value == "true" || value == "false");
    }

    #[test]
    fn generate_value_gender_is_male_or_female() {
        let mut rng = row_rng(42, 1);
        let value = generate_value(&PreparedColumnType::Gender, 1, &mut rng);
        assert!(value == "男性" || value == "女性");
    }

    #[test]
    fn generate_value_blood_type_is_one_of_four_types() {
        let mut rng = row_rng(42, 1);
        let value = generate_value(&PreparedColumnType::BloodType, 1, &mut rng);
        assert!(["A型", "O型", "B型", "AB型"].contains(&value.as_str()));
    }

    #[test]
    fn gender_and_blood_type_are_quoted_as_text_in_sql() {
        assert!(is_text_column(&PreparedColumnType::Gender));
        assert!(is_text_column(&PreparedColumnType::BloodType));
        assert_eq!(sql_literal(&PreparedColumnType::Gender, "男性"), "'男性'");
        assert_eq!(sql_literal(&PreparedColumnType::BloodType, "A型"), "'A型'");
    }

    #[test]
    fn unique_gender_produces_no_duplicates() {
        let schema: Schema = serde_json::from_value(serde_json::json!({
            "row_count": 2,
            "columns": [{ "name": "gender", "type": "gender", "unique": true }]
        }))
        .unwrap();
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 42);
        let rows = generate_all_rows(schema.row_count, &columns, 42);
        let values: std::collections::HashSet<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(values.len(), 2, "genderのunique指定で2行とも異なる値になるはず");
    }

    #[test]
    fn compile_pattern_literal_and_escape() {
        let pieces = compile_pattern(r"AB\-C").unwrap();
        let joined: String = pieces.iter().map(|p| p.chars[0]).collect();
        assert_eq!(joined, "AB-C");
        assert!(pieces.iter().all(|p| p.min_repeat == 1 && p.max_repeat == 1));
    }

    #[test]
    fn compile_pattern_character_class_with_range() {
        let pieces = compile_pattern("[A-C]").unwrap();
        assert_eq!(pieces.len(), 1);
        let mut chars = pieces[0].chars.clone();
        chars.sort();
        assert_eq!(chars, vec!['A', 'B', 'C']);
    }

    #[test]
    fn compile_pattern_negated_class_excludes_listed_chars() {
        let pieces = compile_pattern("[^A-Z]").unwrap();
        assert!(!pieces[0].chars.contains(&'M'));
        assert!(pieces[0].chars.contains(&'a')); // 小文字は除外対象に入れていないので残る
        assert!(pieces[0].chars.contains(&'5'));
    }

    #[test]
    fn compile_pattern_quantifiers() {
        assert_eq!(compile_pattern("A?").unwrap()[0].min_repeat, 0);
        assert_eq!(compile_pattern("A?").unwrap()[0].max_repeat, 1);
        assert_eq!(compile_pattern("A+").unwrap()[0].min_repeat, 1);
        let (min3, max3) = { let p = compile_pattern("A{3}").unwrap(); (p[0].min_repeat, p[0].max_repeat) };
        assert_eq!((min3, max3), (3, 3));
        let (min2, max5) = { let p = compile_pattern("A{2,5}").unwrap(); (p[0].min_repeat, p[0].max_repeat) };
        assert_eq!((min2, max5), (2, 5));
    }

    #[test]
    fn compile_pattern_rejects_dangling_quantifier() {
        assert!(compile_pattern("*").is_err());
    }

    #[test]
    fn compile_pattern_rejects_unclosed_bracket() {
        assert!(compile_pattern("[A-Z").is_err());
    }

    #[test]
    fn compile_pattern_rejects_reversed_range() {
        assert!(compile_pattern("[Z-A]").is_err());
    }

    #[test]
    fn compile_pattern_rejects_empty_pattern() {
        assert!(compile_pattern("").is_err());
    }

    // 回帰テスト: 以前はグループ化"(...)"や選択"a|b"が非対応構文にもかかわらず
    // エラーにも警告にもならず、かっこ・|の記号ごと固定文字列としてそのまま
    // 生成されてしまっていた(ユーザーが意図した繰り返し・二択が全く効かないのに気づきにくい)
    #[test]
    fn compile_pattern_rejects_grouping() {
        assert!(compile_pattern("(abc)").is_err());
    }

    #[test]
    fn compile_pattern_rejects_alternation() {
        assert!(compile_pattern("a|b").is_err());
    }

    // エスケープすれば、かっこ・|自体を1文字のリテラルとして使えることの確認
    // (非対応なのはあくまで「特殊記号としての」グループ化・選択であって、
    // その文字自体を値に含めたい場合の逃げ道は塞がない)
    #[test]
    fn compile_pattern_allows_escaped_grouping_chars_as_literals() {
        let pieces = compile_pattern(r"\(\)\|").unwrap();
        let joined: String = pieces.iter().map(|p| p.chars[0]).collect();
        assert_eq!(joined, "()|");
    }

    #[test]
    fn generate_value_pattern_matches_expected_shape() {
        let pieces = compile_pattern("[A-Z]{3}-[0-9]{4}").unwrap();
        let mut rng = row_rng(42, 1);
        let value = generate_value(&PreparedColumnType::Pattern { pieces }, 1, &mut rng);
        let re_shape = value.len() == 8
            && value.as_bytes()[3] == b'-'
            && value[..3].chars().all(|c| c.is_ascii_uppercase())
            && value[4..].chars().all(|c| c.is_ascii_digit());
        assert!(re_shape, "生成された値がパターンの形になっていない: {value}");
    }

    #[test]
    fn prepare_columns_rejects_invalid_pattern() {
        let schema: Schema = serde_json::from_value(serde_json::json!({
            "row_count": 1,
            "columns": [{ "name": "code", "type": "pattern", "pattern": "[A-Z" }]
        }))
        .unwrap();
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn format_wareki_handles_first_year_as_gannen() {
        let date = chrono::NaiveDate::from_ymd_opt(2019, 5, 1).unwrap();
        assert_eq!(format_wareki(date), "令和元年5月1日");
    }

    #[test]
    fn format_wareki_handles_era_boundary() {
        // 昭和64年は1月7日まで(1月8日から平成元年)
        let showa_last_day = chrono::NaiveDate::from_ymd_opt(1989, 1, 7).unwrap();
        assert_eq!(format_wareki(showa_last_day), "昭和64年1月7日");
        let heisei_first_day = chrono::NaiveDate::from_ymd_opt(1989, 1, 8).unwrap();
        assert_eq!(format_wareki(heisei_first_day), "平成元年1月8日");
    }

    #[test]
    fn format_wareki_handles_ordinary_year() {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 9, 15).unwrap();
        assert_eq!(format_wareki(date), "令和7年9月15日");
    }

    #[test]
    fn row_rng_is_deterministic_for_same_seed_and_row() {
        let kind = PreparedColumnType::Integer { min: 0, max: 1_000_000 };
        let a = generate_value(&kind, 1, &mut row_rng(42, 1));
        let b = generate_value(&kind, 1, &mut row_rng(42, 1));
        assert_eq!(a, b);
    }

    #[test]
    fn random_postal_code_has_nnn_dash_nnnn_format() {
        let mut rng = row_rng(1, 1);
        let code = random_postal_code(&mut rng);
        let parts: Vec<&str> = code.split('-').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3);
        assert_eq!(parts[1].len(), 4);
        assert!(code.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn build_csv_has_header_plus_row_count_lines() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 42).unwrap();
        assert_eq!(csv_text.lines().count(), 6); // ヘッダー1行 + データ5行
    }

    #[test]
    fn null_rate_one_always_produces_empty_csv_field() {
        // name列だけだと「空文字1列の行」と「0列の行」が区別できないため、csvクレートが
        // 空文字を "" とクォートして書き出す。実際のスキーマでは他の列もあるのが普通なので、
        // ここでもid列を足して曖昧さのない状態でテストする
        let schema = schema_from_yaml(
            "row_count: 10\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n    null_rate: 1.0\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 42).unwrap();
        for line in csv_text.lines().skip(1) {
            let name_field = line.split(',').nth(1).unwrap();
            assert_eq!(name_field, "");
        }
    }

    // --- ここから F1-1(enum) / F2-1(json) / F4-1(unique) のテスト ---

    #[test]
    fn company_name_ja_has_kabushiki_gaisha_prefix_and_known_suffix() {
        let schema =
            schema_from_yaml("row_count: 30\ncolumns:\n  - name: c\n    type: company_name_ja\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.starts_with("株式会社"));
            assert!(COMPANY_SUFFIXES.iter().any(|s| line.ends_with(s)));
        }
    }

    #[test]
    fn uuid_column_produces_valid_v4_uuids_without_duplicates() {
        let schema = schema_from_yaml("row_count: 50\ncolumns:\n  - name: id\n    type: uuid\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 42).unwrap();
        let values: Vec<&str> = csv_text.lines().skip(1).collect();
        assert_eq!(values.len(), 50);
        for v in &values {
            assert!(uuid::Uuid::parse_str(v).is_ok(), "invalid uuid: {v}");
        }
        let unique_count = values.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, values.len()); // 50個も作れば衝突しないはず
    }

    #[test]
    fn uuid_generation_is_reproducible_with_same_seed() {
        let a = random_uuid(&mut row_rng(42, 1));
        let b = random_uuid(&mut row_rng(42, 1));
        assert_eq!(a, b);
    }

    #[test]
    fn enum_column_only_produces_declared_choices() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: status\n    type: enum\n    choices: [利用中, 休止中, 退会済み]\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(["利用中", "休止中", "退会済み"].contains(&line));
        }
    }

    #[test]
    fn prepare_columns_rejects_empty_enum_choices() {
        let schema =
            schema_from_yaml("row_count: 1\ncolumns:\n  - name: s\n    type: enum\n    choices: []\n");
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn unique_integer_produces_no_duplicates_and_stays_in_range() {
        let schema = schema_from_yaml(
            "row_count: 20\ncolumns:\n  - name: n\n    type: integer\n    min: 1\n    max: 20\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 42);
        let csv_text = build_csv(schema.row_count, &columns, 42).unwrap();
        let mut values: Vec<i64> = csv_text.lines().skip(1).map(|l| l.parse().unwrap()).collect();
        values.sort_unstable();
        assert_eq!(values, (1..=20).collect::<Vec<i64>>()); // 1〜20が重複なくちょうど1回ずつ出る
    }

    #[test]
    fn prepare_columns_rejects_unique_when_capacity_is_insufficient() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: flag\n    type: boolean\n    unique: true\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    // name_ja(姓30×名20=600通り)はEnumerable方式でuniqueに対応できることを確認する
    #[test]
    fn unique_name_ja_produces_no_duplicate_full_names() {
        let schema = schema_from_yaml(
            "row_count: 100\ncolumns:\n  - name: n\n    type: name_ja\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 7);
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        let names: Vec<&str> = csv_text.lines().skip(1).collect();
        let unique_count = names.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, names.len());
    }

    // last_name_ja単独は30通りしかないので、row_count=30(ちょうど容量いっぱい)でも
    // ユニークな値が過不足なく用意できることを確認する
    #[test]
    fn unique_last_name_ja_produces_no_duplicate_surnames() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: sei\n    type: last_name_ja\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 3);
        let csv_text = build_csv(schema.row_count, &columns, 3).unwrap();
        let mut names: Vec<&str> = csv_text.lines().skip(1).collect();
        names.sort_unstable();
        let mut expected: Vec<&str> = LAST_NAMES.to_vec();
        expected.sort_unstable();
        assert_eq!(names, expected);
    }

    // name_jaの組み合わせ数(600)を超えるrow_countでuniqueを指定するとエラーになることを確認する
    #[test]
    fn prepare_columns_rejects_unique_name_ja_when_row_count_exceeds_capacity() {
        let schema = schema_from_yaml(
            "row_count: 601\ncolumns:\n  - name: n\n    type: name_ja\n    unique: true\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    // phone_ja(携帯電話番号)は組み合わせ数が膨大でEnumerable方式では列挙できないため、
    // Retry方式(値を作って重複チェック)でuniqueに対応できることを確認する
    #[test]
    fn unique_phone_ja_produces_no_duplicates() {
        let schema = schema_from_yaml(
            "row_count: 500\ncolumns:\n  - name: tel\n    type: phone_ja\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 11);
        let csv_text = build_csv(schema.row_count, &columns, 11).unwrap();
        let numbers: Vec<&str> = csv_text.lines().skip(1).collect();
        let unique_count = numbers.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, numbers.len());
        for n in &numbers {
            let prefix = n.split('-').next().unwrap();
            assert!(PHONE_PREFIXES.contains(&prefix), "{n}");
        }
    }

    // 今回unique対応を拡張した型(Enumerable方式・Retry方式あわせて18型。birth_dateは
    // min_age/max_ageの指定が必要なので別テストにしている)について、それぞれ
    // row_count分だけ重複なく生成できることをまとめて確認する。型ごとに個別のテスト関数を
    // 18個書く代わりに、失敗時にどの型かをassertメッセージで分かるようにしている
    #[test]
    fn unique_newly_supported_types_produce_no_duplicates() {
        let cases: &[(&str, u32)] = &[
            ("prefecture_ja", 5),
            ("company_name_ja", 10),
            ("department_ja", 10),
            ("job_title_ja", 10),
            ("credit_card_expiry", 10),
            ("postal_code", 50),
            ("address_ja", 50),
            ("uuid", 50),
            ("ip_address", 50),
            ("jwt", 20),
            ("api_key", 50),
            ("username", 50),
            ("password", 50),
            ("profile_image_url", 50),
            ("credit_card_number", 50),
            ("bank_account_number", 50),
            ("product_sku", 50),
            ("my_number", 50),
        ];

        for (type_name, row_count) in cases {
            let yaml = format!("row_count: {row_count}\ncolumns:\n  - name: v\n    type: {type_name}\n    unique: true\n");
            let schema = schema_from_yaml(&yaml);
            let mut columns =
                prepare_columns(&schema).unwrap_or_else(|e| panic!("{type_name}: prepare_columns失敗: {e}"));
            resolve_unique_pools(&mut columns, schema.row_count, 42);
            let rows = generate_all_rows(schema.row_count, &columns, 42);
            let values: std::collections::HashSet<_> = rows.iter().map(|r| r[0].clone()).collect();
            assert_eq!(values.len(), *row_count as usize, "{type_name}: unique指定で重複が生じた");
        }
    }

    #[test]
    fn unique_birth_date_produces_no_duplicates() {
        let schema = schema_from_yaml(
            "row_count: 10\ncolumns:\n  - name: b\n    type: birth_date\n    min_age: 18\n    max_age: 65\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 42);
        let rows = generate_all_rows(schema.row_count, &columns, 42);
        let values: std::collections::HashSet<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(values.len(), 10, "birth_date: unique指定で重複が生じた");
    }

    #[test]
    fn prepare_columns_rejects_unique_prefecture_ja_when_row_count_exceeds_capacity() {
        // CITIES_BY_PREFECTUREは5都道府県分しか無いため、6行以上のunique指定はエラーになる
        let schema =
            schema_from_yaml("row_count: 6\ncolumns:\n  - name: p\n    type: prefecture_ja\n    unique: true\n");
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_unique_on_unsupported_type() {
        // patternは組み合わせ数の計算が複雑なため非対応のまま(このtypeを非対応の例として使う。
        // fixed/address_ja等は現在Enumerable/Retry方式でそれぞれ対応済み)
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: a\n    type: pattern\n    pattern: \"[A-Z]{3}\"\n    unique: true\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    // fixedは常に同じ値を返す型なので、唯一の組み合わせ(row_count=1)でだけunique指定が成立し、
    // 2行以上を指定すると他のEnumerable型と同じ「組み合わせが足りない」エラーになることを確認する
    #[test]
    fn unique_fixed_allows_row_count_one_but_rejects_more() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: a\n    type: fixed\n    value: x\n    unique: true\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 42);
        let rows = generate_all_rows(schema.row_count, &columns, 42);
        assert_eq!(rows[0][0].as_deref(), Some("x"));

        let schema_two_rows = schema_from_yaml(
            "row_count: 2\ncolumns:\n  - name: a\n    type: fixed\n    value: x\n    unique: true\n",
        );
        assert!(prepare_columns(&schema_two_rows).is_err());
    }

    // floatは範囲・桁数から組み合わせ数を計算し、小さければEnumerable(全列挙)・
    // 大きければRetry(作っては重複チェック)のどちらでも重複なく生成できることを確認する
    #[test]
    fn unique_float_produces_no_duplicates_both_enumerable_and_retry() {
        // 組み合わせが11通り(0.0〜1.0を0.1刻み)しかない、Enumerableになる小さい範囲
        let small_schema = schema_from_yaml(
            "row_count: 11\ncolumns:\n  - name: v\n    type: float\n    min: 0.0\n    max: 1.0\n    decimals: 1\n    unique: true\n",
        );
        let mut columns = prepare_columns(&small_schema).unwrap();
        resolve_unique_pools(&mut columns, small_schema.row_count, 42);
        let rows = generate_all_rows(small_schema.row_count, &columns, 42);
        let values: std::collections::HashSet<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(values.len(), 11, "float(小範囲): unique指定で重複が生じた");

        // 組み合わせが1000万通りあり、Retryになる大きい範囲
        let large_schema = schema_from_yaml(
            "row_count: 50\ncolumns:\n  - name: v\n    type: float\n    min: 0.0\n    max: 1000.0\n    decimals: 2\n    unique: true\n",
        );
        let mut columns = prepare_columns(&large_schema).unwrap();
        resolve_unique_pools(&mut columns, large_schema.row_count, 42);
        let rows = generate_all_rows(large_schema.row_count, &columns, 42);
        let values: std::collections::HashSet<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(values.len(), 50, "float(大範囲): unique指定で重複が生じた");
    }

    #[test]
    fn prepare_columns_rejects_unique_float_when_row_count_exceeds_capacity() {
        // 0.0〜1.0を1.0刻み(decimals:0)だと組み合わせは2通り(0と1)しかない
        let schema = schema_from_yaml(
            "row_count: 3\ncolumns:\n  - name: v\n    type: float\n    min: 0.0\n    max: 1.0\n    decimals: 0\n    unique: true\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_unique_combined_with_null_rate() {
        let schema = schema_from_yaml(
            "row_count: 3\ncolumns:\n  - name: n\n    type: integer\n    min: 1\n    max: 10\n    unique: true\n    null_rate: 0.1\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn prepare_columns_rejects_unique_capacity_over_cap() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: big\n    type: integer\n    min: 0\n    max: 5000000000\n    unique: true\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn build_json_produces_typed_ndjson_lines() {
        let schema = schema_from_yaml(
            "row_count: 3\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n  - name: active\n    type: boolean\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let json_text = build_json(schema.row_count, &columns, 42).unwrap();
        let lines: Vec<&str> = json_text.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(value["id"].is_number());
            assert!(value["name"].is_string());
            assert!(value["active"].is_boolean());
        }
    }

    #[test]
    fn build_json_null_cell_becomes_json_null() {
        let schema = schema_from_yaml(
            "row_count: 10\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n    null_rate: 1.0\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let json_text = build_json(schema.row_count, &columns, 42).unwrap();
        for line in json_text.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(value["name"].is_null());
        }
    }

    // --- ここから F2-2(xlsx) / F3-2(進捗表示) / F4-2(列間整合性) のテスト ---

    #[test]
    fn progress_bar_is_hidden_below_threshold_and_visible_above_it() {
        // is_hidden()は実行環境(ターミナルかどうか)にも左右されてしまうため、
        // 代わりに「バーの長さが設定されているか」でしきい値のロジック自体を確認する
        // (ProgressBar::hidden()は長さを持たない)
        assert_eq!(new_progress_bar(PROGRESS_BAR_THRESHOLD - 1).length(), None);
        assert_eq!(new_progress_bar(PROGRESS_BAR_THRESHOLD).length(), Some(PROGRESS_BAR_THRESHOLD as u64));
    }

    #[test]
    fn city_ja_matches_the_preceding_prefecture_ja_column() {
        let schema = schema_from_yaml(
            "row_count: 50\ncolumns:\n  - name: pref\n    type: prefecture_ja\n  - name: city\n    type: city_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            let pref = parts.next().unwrap();
            let city = parts.next().unwrap();
            let (_, cities) = CITIES_BY_PREFECTURE.iter().find(|(p, _)| *p == pref).unwrap();
            assert!(cities.contains(&city), "{city} is not a city of {pref}");
        }
    }

    #[test]
    fn city_ja_without_prefecture_ja_falls_back_to_any_city() {
        // prefecture_ja列が無い場合でもエラーにならず、どこかの市区町村が選ばれる
        let schema = schema_from_yaml("row_count: 10\ncolumns:\n  - name: city\n    type: city_ja\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(ALL_CITIES.contains(&line));
        }
    }

    #[test]
    fn warns_when_prefecture_ja_comes_after_city_ja() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: city\n    type: city_ja\n  - name: pref\n    type: prefecture_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        assert_eq!(misplaced_city_ja_warnings(&columns).len(), 1);
    }

    #[test]
    fn no_warning_when_prefecture_ja_comes_before_city_ja() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: pref\n    type: prefecture_ja\n  - name: city\n    type: city_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        assert!(misplaced_city_ja_warnings(&columns).is_empty());
    }

    #[test]
    fn no_warning_when_city_ja_used_alone() {
        let schema = schema_from_yaml("row_count: 5\ncolumns:\n  - name: city\n    type: city_ja\n");
        let columns = prepare_columns(&schema).unwrap();
        assert!(misplaced_city_ja_warnings(&columns).is_empty());
    }

    // --- ここから F1-4(katakana_name) のテスト ---

    #[test]
    fn kana_name_arrays_have_same_length_as_name_arrays() {
        assert_eq!(LAST_NAMES.len(), LAST_NAMES_KANA.len());
        assert_eq!(FIRST_NAMES.len(), FIRST_NAMES_KANA.len());
    }

    #[test]
    fn katakana_name_matches_preceding_name_ja() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: name\n    type: name_ja\n  - name: kana\n    type: katakana_name\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            let name = parts.next().unwrap();
            let kana = parts.next().unwrap();
            let last_idx = LAST_NAMES.iter().position(|&n| name.starts_with(n)).unwrap();
            let first_idx = FIRST_NAMES.iter().position(|&n| name.ends_with(n)).unwrap();
            assert_eq!(kana, format!("{}{}", LAST_NAMES_KANA[last_idx], FIRST_NAMES_KANA[first_idx]));
        }
    }

    #[test]
    fn katakana_name_without_name_ja_falls_back() {
        let schema = schema_from_yaml("row_count: 10\ncolumns:\n  - name: kana\n    type: katakana_name\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        assert_eq!(csv_text.lines().skip(1).count(), 10);
    }

    #[test]
    fn warns_when_name_ja_comes_after_katakana_name() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: kana\n    type: katakana_name\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        assert_eq!(misplaced_katakana_name_warnings(&columns).len(), 1);
    }

    #[test]
    fn no_warning_when_name_ja_comes_before_katakana_name() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: name\n    type: name_ja\n  - name: kana\n    type: katakana_name\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        assert!(misplaced_katakana_name_warnings(&columns).is_empty());
    }

    #[test]
    fn romaji_name_matches_preceding_name_ja() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: name\n    type: name_ja\n  - name: romaji\n    type: romaji_name\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            let name = parts.next().unwrap();
            let romaji = parts.next().unwrap();
            let last_idx = LAST_NAMES.iter().position(|&n| name.starts_with(n)).unwrap();
            let first_idx = FIRST_NAMES.iter().position(|&n| name.ends_with(n)).unwrap();
            assert_eq!(romaji, format!("{} {}", LAST_NAMES_ROMAJI[last_idx], FIRST_NAMES_ROMAJI[first_idx]));
        }
    }

    #[test]
    fn romaji_name_without_name_ja_falls_back() {
        let schema = schema_from_yaml("row_count: 10\ncolumns:\n  - name: romaji\n    type: romaji_name\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        assert_eq!(csv_text.lines().skip(1).count(), 10);
    }

    #[test]
    fn warns_when_name_ja_comes_after_romaji_name() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: romaji\n    type: romaji_name\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        assert_eq!(misplaced_katakana_name_warnings(&columns).len(), 1);
    }

    #[test]
    fn katakana_last_name_and_katakana_first_name_only_produce_listed_readings() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: sei\n    type: katakana_last_name\n  - name: mei\n    type: katakana_first_name\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            assert!(LAST_NAMES_KANA.contains(&parts.next().unwrap()));
            assert!(FIRST_NAMES_KANA.contains(&parts.next().unwrap()));
        }
    }

    #[test]
    fn credit_card_expiry_has_mm_slash_yy_format_and_is_in_the_future() {
        use chrono::Datelike;
        let schema = schema_from_yaml("row_count: 50\ncolumns:\n  - name: exp\n    type: credit_card_expiry\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 9).unwrap();
        let this_year_2digit = (chrono::Local::now().date_naive().year() % 100) as u32;
        for line in csv_text.lines().skip(1) {
            let parts: Vec<&str> = line.split('/').collect();
            assert_eq!(parts.len(), 2, "{line}");
            assert_eq!(parts[0].len(), 2, "{line}");
            assert_eq!(parts[1].len(), 2, "{line}");
            let month: u32 = parts[0].parse().unwrap();
            let year: u32 = parts[1].parse().unwrap();
            assert!((1..=12).contains(&month), "{line}");
            assert!(year > this_year_2digit, "{line}"); // 必ず「今年より後」の年になる
        }
    }

    #[test]
    fn bank_account_number_has_seven_digit_format() {
        let schema = schema_from_yaml("row_count: 30\ncolumns:\n  - name: acc\n    type: bank_account_number\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 4).unwrap();
        for line in csv_text.lines().skip(1) {
            assert_eq!(line.len(), 7, "{line}");
            assert!(line.chars().all(|c| c.is_ascii_digit()), "{line}");
        }
    }

    #[test]
    fn product_sku_has_sku_prefix_and_expected_length() {
        let schema = schema_from_yaml("row_count: 30\ncolumns:\n  - name: sku\n    type: product_sku\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 6).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.starts_with("SKU-"), "{line}");
            assert_eq!(line.len(), 12, "{line}"); // "SKU-"(4文字) + 英数字8文字
        }
    }

    // --- ここから F3-3(複数形式同時出力) のテスト ---

    #[test]
    fn output_base_path_single_format_matches_previous_behavior() {
        assert_eq!(output_base_path(None, Format::Csv, false), "output.csv");
        assert_eq!(output_base_path(Some("my.csv"), Format::Csv, false), "my.csv");
        assert_eq!(output_base_path(Some("no_ext"), Format::Json, false), "no_ext");
    }

    #[test]
    fn output_base_path_multiple_formats_derives_extension() {
        assert_eq!(output_base_path(None, Format::Csv, true), "output.csv");
        assert_eq!(output_base_path(None, Format::Json, true), "output.json");
        assert_eq!(output_base_path(Some("result"), Format::Xlsx, true), "result.xlsx");
        assert_eq!(output_base_path(Some("result.csv"), Format::Json, true), "result.json");
    }

    #[test]
    fn build_csv_from_rows_matches_build_csv() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(schema.row_count, &columns, 42);
        assert_eq!(
            build_csv_from_rows(&columns, &rows, false).unwrap(),
            build_csv(schema.row_count, &columns, 42).unwrap()
        );
    }

    #[test]
    fn build_json_from_rows_matches_build_json() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(schema.row_count, &columns, 42);
        assert_eq!(build_json_from_rows(&columns, &rows).unwrap(), build_json(schema.row_count, &columns, 42).unwrap());
    }

    #[test]
    fn write_output_csv_and_json_produce_identical_underlying_data() {
        // --format csv,json のように複数形式を指定したとき、同じ乱数から生成した
        // 1つのrowsを両方の形式に使い回せていることを確認する
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let rows = generate_all_rows(schema.row_count, &columns, 99);
        let csv_text = build_csv_from_rows(&columns, &rows, false).unwrap();
        let json_text = build_json_from_rows(&columns, &rows).unwrap();

        let csv_names: Vec<&str> = csv_text.lines().skip(1).map(|l| l.split(',').nth(1).unwrap()).collect();
        let json_names: Vec<String> = json_text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(csv_names, json_names);
    }

    #[test]
    fn write_xlsx_produces_a_readable_workbook_with_correct_types_and_nulls() {
        let schema = schema_from_yaml(
            "row_count: 8\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n    null_rate: 1.0\n  - name: active\n    type: boolean\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let path = std::env::temp_dir().join("dummy_data_gen_test.xlsx");
        let path_str = path.to_str().unwrap();

        write_xlsx(schema.row_count, &columns, 42, path_str).unwrap();

        use calamine::{DataType, Reader};
        let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path_str).unwrap();
        let range = workbook.worksheet_range_at(0).unwrap().unwrap();
        let rows: Vec<_> = range.rows().collect();

        assert_eq!(rows.len(), 9); // ヘッダー1行 + データ8行
        assert_eq!(rows[0][0].to_string(), "id");

        for row in &rows[1..] {
            assert!(row[0].is_int() || row[0].is_float()); // idは数値
            assert!(row[1].is_empty()); // nameはnull_rate:1.0なので常に空セル
            assert!(row[2].get_bool().is_some()); // activeは真偽値
        }

        let _ = std::fs::remove_file(&path);
    }

    // --- ここからストリーミング書き込み(write_csv_streaming/write_sql_streaming)のテスト ---

    // チャンク境界(row_countがchunk_sizeの倍数でない)をわざと跨ぐ設定で、
    // 一括生成(build_csv)と1バイトも違わないことを確認する
    #[test]
    fn write_csv_streaming_matches_build_csv_across_chunk_boundaries() {
        let schema = schema_from_yaml(
            "row_count: 23\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n  - name: pref\n    type: prefecture_ja\n  - name: city\n    type: city_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let expected = build_csv(schema.row_count, &columns, 42).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming.csv");
        let path_str = path.to_str().unwrap();
        let mut progress_calls = Vec::new();
        write_csv_streaming(schema.row_count, &columns, 42, path_str, Encoding::Utf8, 7, false, false, |done, total| {
            progress_calls.push((done, total));
        })
        .unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(progress_calls.last(), Some(&(23u64, 23u64)));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_csv_streaming_with_zero_rows_writes_header_only() {
        // schema.row_count自体は(prepare_columns_rejects_row_count_zeroの通り)0を
        // 許可しなくなったが、この関数の呼び出し元(GUI側の内部計算等)がrow_countとは
        // 別に0を渡してくる可能性はゼロではないため、write_csv_streaming自体が
        // 0行でもクラッシュせずヘッダーだけ書き出せることは引き続き保証しておく。
        // schemaのrow_countは検証を通すためだけの値(1)にし、実際にstreaming関数へ
        // 渡すrow_countだけを0にする
        let schema = schema_from_yaml("row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n");
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_zero.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(0, &columns, 42, path_str, Encoding::Utf8, 10, false, false, |_, _| {}).unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual.lines().count(), 1); // ヘッダー行のみ
        assert_eq!(actual.lines().next().unwrap(), "id");

        let _ = std::fs::remove_file(&path);
    }

    // write_bom: trueのとき、Excel(日本語版)がUTF-8のCSVをShift-JISと誤認して
    // 文字化けしないよう、ファイル先頭にUTF-8のBOM(EF BB BF)が書き込まれることを確認する
    #[test]
    fn write_csv_streaming_with_bom_true_prepends_utf8_bom() {
        let schema = schema_from_yaml("row_count: 3\ncolumns:\n  - name: id\n    type: sequence\n");
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_bom.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(schema.row_count, &columns, 1, path_str, Encoding::Utf8, 10, true, false, |_, _| {}).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
        assert_eq!(&bytes[3..], "id\n1\n2\n3\n".as_bytes());

        let _ = std::fs::remove_file(&path);
    }

    // write_bom: falseのとき(CLIからの呼び出し)は、これまで通りBOMが付かないことを確認する
    #[test]
    fn write_csv_streaming_with_bom_false_has_no_bom() {
        let schema = schema_from_yaml("row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n");
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_no_bom.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(schema.row_count, &columns, 1, path_str, Encoding::Utf8, 10, false, false, |_, _| {}).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_ne!(&bytes[..3.min(bytes.len())], &[0xEF, 0xBB, 0xBF][..3.min(bytes.len())]);

        let _ = std::fs::remove_file(&path);
    }

    // quote_all: trueのとき、スペースを含む値だけでなく数値の列も含めて全ての値が
    // ダブルクォートで囲まれ、値の中の"は""にエスケープされることを確認する
    #[test]
    fn write_csv_streaming_with_quote_all_true_quotes_every_field() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: fixed\n    value: \"山田 太郎\"\n  - name: note\n    type: fixed\n    value: 'he said \"hi\", ok'\n",
        );
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_quote_all.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(schema.row_count, &columns, 1, path_str, Encoding::Utf8, 10, false, true, |_, _| {}).unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, "\"id\",\"name\",\"note\"\n\"1\",\"山田 太郎\",\"he said \"\"hi\"\", ok\"\n");

        let _ = std::fs::remove_file(&path);
    }

    // quote_all: false(既定)のときは、これまで通り必要な値だけがクォートされることを確認する
    // (数値やスペースだけの値はクォートされない)
    #[test]
    fn write_csv_streaming_with_quote_all_false_only_quotes_when_necessary() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: fixed\n    value: \"山田 太郎\"\n",
        );
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_quote_none.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(schema.row_count, &columns, 1, path_str, Encoding::Utf8, 10, false, false, |_, _| {}).unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, "id,name\n1,山田 太郎\n");

        let _ = std::fs::remove_file(&path);
    }

    // unique制約・列間整合性(prefecture_ja→city_ja, name_ja→katakana_name)が
    // チャンク分割後も壊れていないことを確認する(resolve_unique_poolsは事前に一度だけ呼ぶ)
    #[test]
    fn write_csv_streaming_preserves_unique_and_row_context_across_chunks() {
        let schema = schema_from_yaml(
            "row_count: 37\ncolumns:\n  - name: n\n    type: integer\n    min: 1\n    max: 37\n    unique: true\n  - name: pref\n    type: prefecture_ja\n  - name: city\n    type: city_ja\n  - name: name\n    type: name_ja\n  - name: kana\n    type: katakana_name\n",
        );
        let mut columns = prepare_columns(&schema).unwrap();
        resolve_unique_pools(&mut columns, schema.row_count, 99);

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_unique.csv");
        let path_str = path.to_str().unwrap();
        write_csv_streaming(schema.row_count, &columns, 99, path_str, Encoding::Utf8, 5, false, false, |_, _| {}).unwrap();
        let actual = std::fs::read_to_string(&path).unwrap();

        let mut values: Vec<i64> = Vec::new();
        for line in actual.lines().skip(1) {
            let mut parts = line.split(',');
            values.push(parts.next().unwrap().parse().unwrap());
            let pref = parts.next().unwrap();
            let city = parts.next().unwrap();
            let name = parts.next().unwrap();
            let kana = parts.next().unwrap();
            let (_, cities) = CITIES_BY_PREFECTURE.iter().find(|(p, _)| *p == pref).unwrap();
            assert!(cities.contains(&city), "{city} is not a city of {pref}");
            let last_idx = LAST_NAMES.iter().position(|&n| name.starts_with(n)).unwrap();
            let first_idx = FIRST_NAMES.iter().position(|&n| name.ends_with(n)).unwrap();
            assert_eq!(kana, format!("{}{}", LAST_NAMES_KANA[last_idx], FIRST_NAMES_KANA[first_idx]));
        }
        values.sort_unstable();
        assert_eq!(values, (1..=37).collect::<Vec<i64>>());

        let _ = std::fs::remove_file(&path);
    }

    // chunk_sizeがSQL_BATCH_SIZE(1000)の倍数であれば、INSERT文の区切り方が
    // 一括生成(build_sql)と完全に一致することを確認する(2500行を1000行ずつの
    // チャンクに分けても、内部のバッチ分割境界がズレない)
    #[test]
    fn write_sql_streaming_matches_build_sql_across_chunk_boundaries() {
        let schema = schema_from_yaml(
            "row_count: 2500\ntable_name: t\ncolumns:\n  - name: id\n    type: sequence\n  - name: name\n    type: name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let expected = build_sql(schema.row_count, &columns, "t", 42).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming.sql");
        let path_str = path.to_str().unwrap();
        write_sql_streaming(schema.row_count, &columns, 42, "t", path_str, Encoding::Utf8, 1000, |_, _| {}).unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.matches("INSERT INTO").count(), 3); // 1000, 1000, 500行の3本

        let _ = std::fs::remove_file(&path);
    }

    // chunk_sizeがSQL_BATCH_SIZE(1000)の倍数でない場合、一括生成とはINSERT文の
    // 区切り方が変わる(チャンクごとに1本のINSERTになる)。壊れているわけではなく、
    // 「chunk_sizeは1000の倍数にする」という前提を守らなかった場合の挙動として記録しておく
    #[test]
    fn write_sql_streaming_produces_one_insert_per_chunk_when_chunk_size_is_not_a_multiple_of_batch_size() {
        let schema = schema_from_yaml(
            "row_count: 15\ntable_name: t\ncolumns:\n  - name: id\n    type: sequence\n",
        );
        let columns = prepare_columns(&schema).unwrap();

        let path = std::env::temp_dir().join("dummy_data_gen_test_streaming_small_batch.sql");
        let path_str = path.to_str().unwrap();
        write_sql_streaming(schema.row_count, &columns, 1, "t", path_str, Encoding::Utf8, 4, |_, _| {}).unwrap();

        let actual = std::fs::read_to_string(&path).unwrap();
        assert_eq!(actual.matches("INSERT INTO").count(), 4); // 15行 ÷ chunk_size(4) = 4,4,4,3の4チャンク

        let _ = std::fs::remove_file(&path);
    }

    // --- ここから新規列タイプ(固定値テキスト/メールドメイン/固定電話番号/姓名単独/
    // フリガナ半角/生年月日年齢範囲/日付フォーマット)のテスト ---

    #[test]
    fn fixed_column_always_returns_the_configured_value() {
        let schema =
            schema_from_yaml("row_count: 10\ncolumns:\n  - name: c\n    type: fixed\n    value: 常に同じ値\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert_eq!(line, "常に同じ値");
        }
    }

    #[test]
    fn email_without_domain_field_defaults_to_example_com() {
        // 既存のschema.yaml(domain未指定)との後方互換を確認する
        let schema = schema_from_yaml("row_count: 3\ncolumns:\n  - name: e\n    type: email\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.ends_with("@example.com"), "{line}");
        }
    }

    #[test]
    fn email_with_custom_domain_uses_that_domain() {
        let schema = schema_from_yaml(
            "row_count: 3\ncolumns:\n  - name: e\n    type: email\n    domain: mycompany.co.jp\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.ends_with("@mycompany.co.jp"), "{line}");
        }
    }

    #[test]
    fn phone_ja_landline_has_prefix_dash_nnnn_dash_nnnn_format() {
        let schema =
            schema_from_yaml("row_count: 30\ncolumns:\n  - name: p\n    type: phone_ja_landline\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 3).unwrap();
        for line in csv_text.lines().skip(1) {
            let parts: Vec<&str> = line.split('-').collect();
            assert_eq!(parts.len(), 3, "{line}");
            assert!(PHONE_PREFIXES_LANDLINE.contains(&parts[0]), "{line}");
        }
    }

    #[test]
    fn name_ja_with_space_true_inserts_space_between_surname_and_given_name() {
        let schema = schema_from_yaml(
            "row_count: 10\ncolumns:\n  - name: n\n    type: name_ja\n    with_space: true\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.splitn(2, ' ');
            assert!(LAST_NAMES.contains(&parts.next().unwrap()), "{line}");
            assert!(FIRST_NAMES.contains(&parts.next().unwrap()), "{line}");
        }
    }

    // with_spaceを省略した場合は、既存のschema.yamlとの後方互換のため今まで通り
    // スペース無しで出力されることを確認する
    #[test]
    fn name_ja_without_with_space_field_defaults_to_no_space() {
        let schema = schema_from_yaml("row_count: 10\ncolumns:\n  - name: n\n    type: name_ja\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(!line.contains(' '), "{line}");
        }
    }

    #[test]
    fn last_name_ja_and_first_name_ja_only_produce_listed_names() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: sei\n    type: last_name_ja\n  - name: mei\n    type: first_name_ja\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            assert!(LAST_NAMES.contains(&parts.next().unwrap()));
            assert!(FIRST_NAMES.contains(&parts.next().unwrap()));
        }
    }

    #[test]
    fn hankaku_katakana_table_covers_all_dictionary_characters() {
        for name in LAST_NAMES_KANA.iter().chain(FIRST_NAMES_KANA.iter()) {
            for c in name.chars() {
                assert!(
                    KATAKANA_FULL_TO_HALF.iter().any(|(full, _)| *full == c),
                    "文字 '{c}' (name: {name}) の半角変換表が無い"
                );
            }
        }
    }

    #[test]
    fn katakana_name_hankaku_matches_preceding_name_ja_in_half_width() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: name\n    type: name_ja\n  - name: kana\n    type: katakana_name_hankaku\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            let name = parts.next().unwrap();
            let kana = parts.next().unwrap();
            let last_idx = LAST_NAMES.iter().position(|&n| name.starts_with(n)).unwrap();
            let first_idx = FIRST_NAMES.iter().position(|&n| name.ends_with(n)).unwrap();
            let expected_full = format!("{}{}", LAST_NAMES_KANA[last_idx], FIRST_NAMES_KANA[first_idx]);
            assert_eq!(kana, to_hankaku_katakana(&expected_full));
        }
    }

    #[test]
    fn katakana_name_with_space_true_inserts_space_between_surname_and_given_name() {
        let schema = schema_from_yaml(
            "row_count: 10\ncolumns:\n  - name: kana\n    type: katakana_name\n    with_space: true\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.splitn(2, ' ');
            assert!(LAST_NAMES_KANA.contains(&parts.next().unwrap()), "{line}");
            assert!(FIRST_NAMES_KANA.contains(&parts.next().unwrap()), "{line}");
        }
    }

    // with_spaceを省略した場合は、既存のschema.yamlとの後方互換のため今まで通り
    // スペース無しで出力されることを確認する(name_ja側の同名テストと同じ考え方)
    #[test]
    fn katakana_name_without_with_space_field_defaults_to_no_space() {
        let schema = schema_from_yaml("row_count: 10\ncolumns:\n  - name: kana\n    type: katakana_name\n");
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 5).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(!line.contains(' '), "{line}");
        }
    }

    #[test]
    fn katakana_name_hankaku_with_space_true_inserts_half_width_space() {
        let schema = schema_from_yaml(
            "row_count: 30\ncolumns:\n  - name: name\n    type: name_ja\n  - name: kana\n    type: katakana_name_hankaku\n    with_space: true\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 7).unwrap();
        for line in csv_text.lines().skip(1) {
            let mut parts = line.split(',');
            let name = parts.next().unwrap();
            let kana = parts.next().unwrap();
            let last_idx = LAST_NAMES.iter().position(|&n| name.starts_with(n)).unwrap();
            let first_idx = FIRST_NAMES.iter().position(|&n| name.ends_with(n)).unwrap();
            let expected = format!(
                "{} {}",
                to_hankaku_katakana(LAST_NAMES_KANA[last_idx]),
                to_hankaku_katakana(FIRST_NAMES_KANA[first_idx])
            );
            assert_eq!(kana, expected);
        }
    }

    #[test]
    fn birth_date_stays_within_configured_age_range() {
        let schema = schema_from_yaml(
            "row_count: 50\ncolumns:\n  - name: b\n    type: birth_date\n    min_age: 20\n    max_age: 30\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 11).unwrap();
        let today = chrono::Local::now().date_naive();
        for line in csv_text.lines().skip(1) {
            let date = chrono::NaiveDate::parse_from_str(line, "%Y-%m-%d").unwrap();
            let age_days = (today - date).num_days();
            // 365日/年の近似計算のため、境界に±1年程度の遊びを持たせて検証する
            assert!(age_days >= 19 * 365 && age_days <= 31 * 365, "{line} (age_days={age_days})");
        }
    }

    #[test]
    fn prepare_columns_rejects_birth_date_min_age_greater_than_max_age() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: b\n    type: birth_date\n    min_age: 30\n    max_age: 20\n",
        );
        assert!(prepare_columns(&schema).is_err());
    }

    #[test]
    fn date_format_slash_uses_yyyy_slash_mm_slash_dd() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: d\n    type: date\n    start: \"2020-01-01\"\n    end: \"2020-01-31\"\n    format: slash\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.starts_with("2020/01/"), "{line}");
        }
    }

    #[test]
    fn date_without_format_field_defaults_to_ymd() {
        // 既存のschema.yaml(format未指定)との後方互換を確認する
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: d\n    type: date\n    start: \"2020-01-01\"\n    end: \"2020-01-31\"\n",
        );
        let columns = prepare_columns(&schema).unwrap();
        let csv_text = build_csv(schema.row_count, &columns, 1).unwrap();
        for line in csv_text.lines().skip(1) {
            assert!(line.starts_with("2020-01-"), "{line}");
        }
    }
}
