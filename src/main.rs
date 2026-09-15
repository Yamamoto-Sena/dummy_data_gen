use chrono::Datelike;
use clap::{Parser, ValueEnum};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// 「このプログラムが受け取れる引数はこれです」という設計図(struct)
#[derive(Parser)]
struct Args {
    /// 列の定義を書いたYAMLファイルのパス
    #[arg(long, default_value = "schema.yaml")]
    config: String,

    /// 出力する文字コード(utf8: 通常のUTF-8 / sjis: Shift-JIS。Dr.SumやMotionBoardなど
    /// 日本の業務システムはShift-JISを前提にしていることが多いため用意している)
    #[arg(long, value_enum, default_value_t = Encoding::Utf8)]
    encoding: Encoding,

    /// 出力形式(csv / sql / json / xlsx)。カンマ区切りで複数指定すると1回の実行で全部出力する
    /// (例: --format csv,json)。sqlの場合はschema.yamlに table_name の指定が必要。
    /// xlsxの場合は--encodingが無視される(Excelは常にUTF-8相当の内部形式のため)
    #[arg(long, value_enum, value_delimiter = ',', default_value = "csv")]
    format: Vec<Format>,

    /// 乱数のシード値。指定すると、同じ設定なら毎回同じデータが再現される(テストや再実行に便利)。
    /// 省略した場合は毎回ランダムなシードを使う
    #[arg(long)]
    seed: Option<u64>,

    /// 出力先ファイルパス。省略時は形式に応じて output.csv / output.sql / output.json / output.xlsx に保存する。
    /// --formatを複数指定したときは拡張子なしのベース名として扱い、形式ごとに拡張子を付ける
    /// (例: --output result --format csv,json → result.csv / result.json)
    #[arg(long)]
    output: Option<String>,
}

// 行番号ごとに独立したRNGを作る。base_seedが同じなら常に同じ値になるため、
// 並列実行(rayon)でどのスレッドがどの行を処理しても結果が変わらない。
// SmallRng(暗号強度は無いが高速なPRNG)を使うのは、ダミーデータ生成に暗号学的な安全性は不要なため。
fn row_rng(base_seed: u64, row_num: u32) -> SmallRng {
    SmallRng::seed_from_u64(base_seed.wrapping_add(row_num as u64))
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq, Hash)]
enum Format {
    #[value(name = "csv")]
    Csv,
    #[value(name = "sql")]
    Sql,
    #[value(name = "json")]
    Json,
    #[value(name = "xlsx")]
    Xlsx,
}

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

// schema.yaml の中身をそのまま受け止める箱(struct)。serdeが自動でYAML→structに変換する。
// 「1テーブル分の定義」を表す型で、単一テーブル形式(schema.yamlのトップレベルに
// row_count/columnsを直接書く形式)でも、複数テーブル形式(tables:の各要素)でも、
// どちらも同じこの型として読み込む。tables:形式では要素のキーが"name"なので、
// aliasでtable_nameとしても受け取れるようにしている。
#[derive(Deserialize)]
struct Schema {
    row_count: u32,
    // SQL出力(--format sql)のときや、tables:形式でのテーブル名として使う
    #[serde(default, alias = "name")]
    table_name: Option<String>,
    columns: Vec<ColumnDef>,
}

// YAMLの最上位をいったん全部Optionとして受け止める箱。単一テーブル形式(row_count/columns
// が直接トップレベルにある)と複数テーブル形式(tables:のリスト)のどちらで書かれているかを
// ここではまだ判定しない(判定はnormalize_schema_fileで行う)。
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
struct SchemaFile {
    tables: Vec<Schema>,
    multi_table: bool,
}

fn normalize_schema_file(raw: RawSchemaFile) -> Result<SchemaFile, Box<dyn std::error::Error>> {
    let has_single_table_fields =
        raw.row_count.is_some() || raw.columns.is_some() || raw.table_name.is_some();

    if let Some(tables) = raw.tables {
        if has_single_table_fields {
            return Err(
                "tables: と row_count:/columns:/table_name: は同時に指定できません。複数テーブルを作る場合は各テーブルの定義を tables: の中に書いてください".into(),
            );
        }
        if tables.is_empty() {
            return Err("tables には少なくとも1つ以上のテーブルを定義してください".into());
        }
        for (i, table) in tables.iter().enumerate() {
            if table.table_name.as_deref().is_none_or(str::is_empty) {
                return Err(format!("tables[{}]: テーブル名(name)を指定してください", i).into());
            }
        }
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
        return Ok(SchemaFile { tables, multi_table: true });
    }

    let columns = raw
        .columns
        .ok_or("columns には少なくとも1つ以上の列を定義してください")?;
    let row_count = raw.row_count.ok_or("row_count を指定してください")?;

    Ok(SchemaFile {
        tables: vec![Schema { row_count, table_name: raw.table_name, columns }],
        multi_table: false,
    })
}

#[derive(Deserialize)]
struct ColumnDef {
    name: String,
    // 0.0〜1.0の確率でNULL(空)を混ぜる。省略時はNULLを混ぜない(0.0)
    #[serde(default)]
    null_rate: Option<f64>,
    // trueにすると、この列の値が行間で重複しないようにする。省略時はfalse
    #[serde(default)]
    unique: Option<bool>,
    // flattenにより、typeやmin/maxなどの追加情報を「nameと同じ階層」から直接読み取れる
    #[serde(flatten)]
    column_type: ColumnType,
}

// tag = "type" にすると、YAML上の "type:" の値でどのバリアント(列タイプ)かを判定し、
// min/maxなどの残りのフィールドをそのバリアントの中身として読み取ってくれる
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ColumnType {
    Sequence,
    NameJa,
    Email,
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
    Date {
        start: String,
        end: String,
    },
    PostalCode,
    PhoneJa,
    AddressJa,
    CompanyNameJa,
    Uuid,
    PrefectureJa,
    CityJa,
    KatakanaName,
    Enum {
        choices: Vec<String>,
    },
    // "テーブル名.列名" 形式(最初の"."で分割)。複数テーブル形式(tables:)でのみ使える
    ForeignKey {
        references: String,
    },
}

fn default_decimals() -> u32 {
    2
}

fn load_schema(path: &str) -> Result<SchemaFile, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("スキーマファイル({})の読み込みに失敗しました: {}", path, e))?;
    let raw: RawSchemaFile = serde_yaml::from_str(&text)?;
    normalize_schema_file(raw)
}

// YAMLから読んだそのままの定義(ColumnType)を、実際に値を作るときに必要な形に変換したもの。
// 日付は文字列のままだと1行作るたびに毎回パースし直すことになり10万行規模で無駄なので、
// ここで先に1回だけ計算しておく。min > max のような矛盾も、生成が始まる前にここで弾く。
struct PreparedColumn {
    name: String,
    kind: PreparedColumnType,
    null_rate: f64,
    unique: bool,
    // uniqueなら、あらかじめ計算しておいた「行数分の重複しない値」がここに入る
    // (resolve_unique_pools が base_seed が決まった後に埋める)。
    // Some(pool)のとき、generate_cellはこの中から順番に値を取り出すだけになる
    unique_pool: Option<Vec<String>>,
}

enum PreparedColumnType {
    Sequence,
    NameJa,
    Email,
    Integer { min: i64, max: i64 },
    Float { min: f64, max: f64, decimals: u32 },
    Boolean,
    Date { start_days: i32, span_days: i64 },
    PostalCode,
    PhoneJa,
    AddressJa,
    CompanyNameJa,
    Uuid,
    PrefectureJa,
    CityJa,
    KatakanaName,
    Enum { choices: Vec<String> },
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

// 外部キーの値を、出力形式ごとにどう扱うか(参照先の列タイプから決まる)
#[derive(Clone, Copy, PartialEq)]
enum FkRepr {
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

fn prepare_columns(schema: &Schema) -> Result<Vec<PreparedColumn>, Box<dyn std::error::Error>> {
    if schema.columns.is_empty() {
        return Err("columns には少なくとも1つ以上の列を定義してください".into());
    }

    for i in 0..schema.columns.len() {
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

    schema
        .columns
        .iter()
        .map(|c| {
            let kind = match &c.column_type {
                ColumnType::Sequence => PreparedColumnType::Sequence,
                ColumnType::NameJa => PreparedColumnType::NameJa,
                ColumnType::Email => PreparedColumnType::Email,
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
                ColumnType::Date { start, end } => {
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
                    }
                }
                ColumnType::PostalCode => PreparedColumnType::PostalCode,
                ColumnType::PhoneJa => PreparedColumnType::PhoneJa,
                ColumnType::AddressJa => PreparedColumnType::AddressJa,
                ColumnType::CompanyNameJa => PreparedColumnType::CompanyNameJa,
                ColumnType::Uuid => PreparedColumnType::Uuid,
                ColumnType::PrefectureJa => PreparedColumnType::PrefectureJa,
                ColumnType::CityJa => PreparedColumnType::CityJa,
                ColumnType::KatakanaName => PreparedColumnType::KatakanaName,
                ColumnType::Enum { choices } => {
                    if choices.is_empty() {
                        return Err(format!("列 \"{}\": choices には1つ以上の選択肢が必要です", c.name).into());
                    }
                    PreparedColumnType::Enum { choices: choices.clone() }
                }
                ColumnType::ForeignKey { references } => {
                    let (ref_table, ref_column) = parse_reference(references, &c.name)?;
                    PreparedColumnType::ForeignKey { ref_table, ref_column, repr: FkRepr::Text, pool: None }
                }
            };

            let null_rate = c.null_rate.unwrap_or(0.0);
            if !(0.0..=1.0).contains(&null_rate) {
                return Err(format!(
                    "列 \"{}\": null_rate({})は0.0〜1.0の範囲で指定してください",
                    c.name, null_rate
                )
                .into());
            }

            let unique = c.unique.unwrap_or(false);
            if unique {
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
                match unique_capacity(&kind) {
                    None => {
                        return Err(format!(
                            "列 \"{}\": このtypeはuniqueに対応していません(enum/boolean/integer/dateのみ対応)",
                            c.name
                        )
                        .into());
                    }
                    Some(capacity) if capacity > UNIQUE_CAPACITY_CAP => {
                        return Err(format!(
                            "列 \"{}\": unique: 値の組み合わせが{}通りあり、上限({}通り)を超えています。範囲や選択肢を絞ってください",
                            c.name, capacity, UNIQUE_CAPACITY_CAP
                        )
                        .into());
                    }
                    Some(capacity) if capacity < schema.row_count as u128 => {
                        return Err(format!(
                            "列 \"{}\": unique: 値の組み合わせが{}通りしかなく、row_count({})分のユニークな値を用意できません",
                            c.name, capacity, schema.row_count
                        )
                        .into());
                    }
                    Some(_) => {}
                }
            }

            // 実際の値(unique_pool)はこの後base_seedが決まってから resolve_unique_pools で埋める
            Ok(PreparedColumn { name: c.name.clone(), kind, null_rate, unique, unique_pool: None })
        })
        .collect::<Result<Vec<_>, _>>()
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
fn misplaced_katakana_name_warnings(columns: &[PreparedColumn]) -> Vec<String> {
    columns
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.kind, PreparedColumnType::KatakanaName))
        .filter(|(kana_idx, _)| {
            let has_preceding_name =
                columns[..*kana_idx].iter().any(|c| matches!(c.kind, PreparedColumnType::NameJa));
            let has_following_name =
                columns[*kana_idx + 1..].iter().any(|c| matches!(c.kind, PreparedColumnType::NameJa));
            !has_preceding_name && has_following_name
        })
        .map(|(_, kana_col)| {
            format!(
                "警告: 列 \"{}\"(katakana_name)より後ろに name_ja 列があります。katakana_nameは自分より前のname_ja列しか参照しないため、氏名とフリガナが対応しません。name_ja列をkatakana_name列より前に移動してください。",
                kana_col.name
            )
        })
        .collect()
}

// unique制約を付けられる列タイプが取りうる値の組み合わせ数。
// 全部の組み合わせをメモリ上に列挙してシャッフルする方式を取るため、これが分かる型だけに対応する。
// postal_code/phone_ja/address_ja/floatは組み合わせが多すぎる、または不連続で数えにくいため非対応
// (これらの値の重複を避けたい場合は、より小さい組み合わせ数のenum/integerで代用することを想定している)。
fn unique_capacity(kind: &PreparedColumnType) -> Option<u128> {
    match kind {
        PreparedColumnType::Boolean => Some(2),
        PreparedColumnType::Integer { min, max } => Some((*max as i128 - *min as i128 + 1) as u128),
        PreparedColumnType::Date { span_days, .. } => Some(*span_days as u128 + 1),
        PreparedColumnType::Enum { choices } => Some(choices.len() as u128),
        _ => None,
    }
}

// unique_capacityがこれを超える場合はエラーにする。組み合わせ全部をVecに列挙するので、
// メモリを使いすぎない(や、あまりに時間がかかりすぎない)ようにするための安全弁
const UNIQUE_CAPACITY_CAP: u128 = 2_000_000;

// unique_capacityで数えた組み合わせを、実際の文字列としてすべて列挙する
fn enumerate_values(kind: &PreparedColumnType) -> Vec<String> {
    match kind {
        PreparedColumnType::Boolean => vec!["true".to_string(), "false".to_string()],
        PreparedColumnType::Integer { min, max } => (*min..=*max).map(|v| v.to_string()).collect(),
        PreparedColumnType::Date { start_days, span_days } => (0..=*span_days)
            .map(|offset| {
                chrono::NaiveDate::from_num_days_from_ce_opt(start_days + offset as i32)
                    .expect("span_daysの範囲内なので必ず有効な日付になる")
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect(),
        PreparedColumnType::Enum { choices } => choices.clone(),
        _ => unreachable!("unique_capacityがNoneを返す型はここに来ない(prepare_columnsで弾いている)"),
    }
}

// 列の全候補値をシャッフルして先頭row_count個を取る=「行数分の重複しない値」の完成。
// column_saltは、同じschema内に複数のunique列があるときに、それぞれ違う乱数列になるようにするための値
fn build_unique_pool(kind: &PreparedColumnType, row_count: u32, base_seed: u64, column_salt: u64) -> Vec<String> {
    let mut values = enumerate_values(kind);
    let mut rng = SmallRng::seed_from_u64(base_seed.wrapping_add(column_salt));
    values.shuffle(&mut rng);
    values.truncate(row_count as usize);
    values
}

// unique指定のある列すべてに対して、実際の値のプールを計算してPreparedColumnに詰める。
// base_seedが決まった後(=prepare_columnsの後)でないと呼べない
fn resolve_unique_pools(columns: &mut [PreparedColumn], row_count: u32, base_seed: u64) {
    for (i, column) in columns.iter_mut().enumerate() {
        if column.unique {
            column.unique_pool = Some(build_unique_pool(&column.kind, row_count, base_seed, i as u64));
        }
    }
}

// SchemaFile.tablesの1テーブル分を、prepare_columns済みの状態にしたもの
struct PreparedTable {
    // 単一テーブル形式(tables:を使っていない)でtable_name未指定ならNone
    name: Option<String>,
    row_count: u32,
    columns: Vec<PreparedColumn>,
}

// SchemaFileの各テーブルにprepare_columnsを適用する
fn prepare_tables(file: &SchemaFile) -> Result<Vec<PreparedTable>, Box<dyn std::error::Error>> {
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
type ColumnKey = (String, String);

// 全FK列の参照先(テーブル/列の存在)を検証し、依存辺(deps[子テーブルindex] = 親テーブルindexのVec)と、
// 「後でプールを取り出す必要がある親の列」(referenced: (テーブル名, 列名) → 参照元の説明)を作る。
#[allow(clippy::type_complexity)] // (Vec<Vec<usize>>, HashMap<...>) は内部専用の戻り値で、これ以上分ける必要は薄い
fn resolve_foreign_keys(
    tables: &mut [PreparedTable],
) -> Result<(Vec<Vec<usize>>, HashMap<ColumnKey, String>), Box<dyn std::error::Error>> {
    let name_to_index: HashMap<&str, usize> = tables
        .iter()
        .enumerate()
        .filter_map(|(i, t)| t.name.as_deref().map(|n| (n, i)))
        .collect();

    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); tables.len()];
    let mut referenced: HashMap<ColumnKey, String> = HashMap::new();

    for (child_idx, table) in tables.iter().enumerate() {
        let child_name = table.name.as_deref().unwrap_or("");
        for column in &table.columns {
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

            let Some(&parent_idx) = name_to_index.get(ref_table.as_str()) else {
                return Err(format!(
                    "列 \"{}\": 参照先のテーブル \"{}\" が tables に定義されていません",
                    column.name, ref_table
                )
                .into());
            };

            let parent = &tables[parent_idx];
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

            if !deps[child_idx].contains(&parent_idx) {
                deps[child_idx].push(parent_idx);
            }
            referenced
                .entry((ref_table.clone(), ref_column.clone()))
                .or_insert_with(|| format!("{}.{}", child_name, column.name));
        }
    }

    Ok((deps, referenced))
}

// Kahnのアルゴリズムで親→子の順に並べる。循環していたら具体的な循環パスを1つ再構成してエラーにする
fn topological_order(
    deps: &[Vec<usize>],
    tables: &[PreparedTable],
) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let n = tables.len();
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

    let mut queue: std::collections::VecDeque<usize> =
        (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);

    while let Some(i) = queue.pop_front() {
        order.push(i);
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

// トポロジカル順に走査してreprを確定させる。多段参照(c.b_id → b.a_id → a.id)で、
// bのreprが確定してからcのreprを決めるために、順不同ではなくトポロジカル順で処理する
fn resolve_fk_reprs(
    tables: &mut [PreparedTable],
    order: &[usize],
) -> Result<(), Box<dyn std::error::Error>> {
    for &i in order {
        // (子テーブル内の列index, 新しいrepr) を先に集めてから書き込む
        // (同じtables[i]の中で複数のFK列があっても、他のテーブルは参照しないのでここは1テーブル完結)
        let mut updates = Vec::new();
        for (col_idx, column) in tables[i].columns.iter().enumerate() {
            if let PreparedColumnType::ForeignKey { ref_table, ref_column, .. } = &column.kind {
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
fn table_seed(base_seed: u64, table_index: usize) -> u64 {
    base_seed.wrapping_add((table_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

// 親テーブル生成後、参照されている列の値をプール化する
fn collect_key_pools(
    table: &PreparedTable,
    rows: &[Vec<Option<String>>],
    referenced: &HashMap<ColumnKey, String>,
    pools: &mut HashMap<ColumnKey, Arc<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(table_name) = table.name.as_deref() else {
        return Ok(());
    };

    for (col_idx, column) in table.columns.iter().enumerate() {
        let key = (table_name.to_string(), column.name.clone());
        let Some(referenced_by) = referenced.get(&key) else {
            continue;
        };

        let values: Vec<String> = rows.iter().filter_map(|row| row[col_idx].clone()).collect();
        if values.is_empty() {
            return Err(format!(
                "テーブル \"{}\" の列 \"{}\" に値が1つもないため、これを参照する {} の値を決められません(row_count が0になっていないか確認してください)",
                table_name, column.name, referenced_by
            )
            .into());
        }

        pools.insert(key, Arc::new(values));
    }

    Ok(())
}

// 子テーブルの生成直前に、FK列にプールを差し込む
fn fill_foreign_key_pools(
    columns: &mut [PreparedColumn],
    pools: &HashMap<ColumnKey, Arc<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    for column in columns.iter_mut() {
        if let PreparedColumnType::ForeignKey { ref_table, ref_column, pool, .. } = &mut column.kind {
            let key = (ref_table.clone(), ref_column.clone());
            let found = pools.get(&key).cloned().expect("トポロジカル順に生成しているので親のプールは必ず存在する");
            *pool = Some(found);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum Encoding {
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

const LAST_NAMES: &[&str] = &["佐藤", "鈴木", "高橋", "田中", "伊藤"];
const FIRST_NAMES: &[&str] = &["翔太", "陽菜", "大輝", "美咲", "健太"];
// LAST_NAMES/FIRST_NAMESと添字が1対1で対応するカタカナ読み(フリガナ)。
// 同じ添字を使うことで、katakana_name列がname_ja列と同じ氏名の読みを返せるようにしている
const LAST_NAMES_KANA: &[&str] = &["サトウ", "スズキ", "タカハシ", "タナカ", "イトウ"];
const FIRST_NAMES_KANA: &[&str] = &["ショウタ", "ヒナ", "ダイキ", "ミサキ", "ケンタ"];
const PHONE_PREFIXES: &[&str] = &["090", "080", "070"];
const COMPANY_SUFFIXES: &[&str] = &["商事", "商会", "工業", "産業", "建設", "システム", "フーズ", "物流"];

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

fn format_name(last_idx: usize, first_idx: usize) -> String {
    format!("{}{}", LAST_NAMES[last_idx], FIRST_NAMES[first_idx])
}

fn random_name(rng: &mut impl Rng) -> String {
    let (last_idx, first_idx) = random_name_indices(rng);
    format_name(last_idx, first_idx)
}

// context(前の列のname_ja)がSomeなら、その氏名と同じ添字のカタカナ読みを返す。
// Noneなら独自にランダムな氏名の読みを作る(katakana_name単独使用時のフォールバック)
fn random_katakana_name(rng: &mut impl Rng, context: Option<(usize, usize)>) -> String {
    let (last_idx, first_idx) = context.unwrap_or_else(|| random_name_indices(rng));
    format!("{}{}", LAST_NAMES_KANA[last_idx], FIRST_NAMES_KANA[first_idx])
}

fn random_email(id: u32) -> String {
    format!("user{}@example.com", id)
}

fn random_postal_code(rng: &mut impl Rng) -> String {
    format!("{:03}-{:04}", rng.gen_range(0..1000), rng.gen_range(0..10000))
}

fn random_phone(rng: &mut impl Rng) -> String {
    let prefix = PHONE_PREFIXES[rng.gen_range(0..PHONE_PREFIXES.len())];
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
// 戻り値の2つ目は「name_ja列として新たに選んだ姓・名の添字」(それ以外の列やNULL・
// unique_pool経由の場合はNone)で、generate_rowがRowContextに保存するために使う。
fn generate_cell(
    column: &PreparedColumn,
    row_num: u32,
    rng: &mut impl Rng,
    ctx: &RowContext,
) -> (Option<String>, Option<(usize, usize)>) {
    // uniqueな列は、あらかじめ用意しておいたプールからこの行番号に対応する値を取り出すだけ
    // (unique同士でnull_rateとの併用はprepare_columnsで禁止しているので、Noneになることは無い)
    if let Some(pool) = &column.unique_pool {
        return (Some(pool[(row_num - 1) as usize].clone()), None);
    }

    if column.null_rate > 0.0 && rng.gen_bool(column.null_rate) {
        return (None, None);
    }

    match column.kind {
        PreparedColumnType::CityJa => (Some(random_city(rng, ctx.last_prefecture.as_deref())), None),
        PreparedColumnType::KatakanaName => {
            (Some(random_katakana_name(rng, ctx.last_name_indices)), None)
        }
        PreparedColumnType::NameJa => {
            let (last_idx, first_idx) = random_name_indices(rng);
            (Some(format_name(last_idx, first_idx)), Some((last_idx, first_idx)))
        }
        _ => (Some(generate_value(&column.kind, row_num, rng)), None),
    }
}

// 1行分(全列)の値を作る。列は前から順番に処理し、prefecture_ja/name_ja列の値を
// RowContextに覚えておいて後ろの列(city_ja/katakana_name)に渡す
// (どちらも「参照される側」の列が「参照する側」の列より前に定義されている必要がある)
fn generate_row(columns: &[PreparedColumn], row_num: u32, rng: &mut impl Rng) -> Vec<Option<String>> {
    let mut ctx = RowContext::default();
    let mut values = Vec::with_capacity(columns.len());

    for column in columns {
        let (cell, name_indices) = generate_cell(column, row_num, rng, &ctx);
        match column.kind {
            PreparedColumnType::PrefectureJa => ctx.last_prefecture = cell.clone(),
            PreparedColumnType::NameJa => ctx.last_name_indices = name_indices,
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
        PreparedColumnType::NameJa => random_name(rng),
        PreparedColumnType::Email => random_email(row_num),
        PreparedColumnType::Integer { min, max } => rng.gen_range(*min..=*max).to_string(),
        PreparedColumnType::Float { min, max, decimals } => {
            let value: f64 = rng.gen_range(*min..=*max);
            format!("{:.*}", *decimals as usize, value)
        }
        PreparedColumnType::Boolean => rng.gen_bool(0.5).to_string(),
        PreparedColumnType::Date { start_days, span_days } => {
            let offset = if *span_days == 0 { 0 } else { rng.gen_range(0..=*span_days) };
            let date = chrono::NaiveDate::from_num_days_from_ce_opt(*start_days + offset as i32)
                .expect("日付の範囲はprepare_columnsで検証済み");
            date.format("%Y-%m-%d").to_string()
        }
        PreparedColumnType::PostalCode => random_postal_code(rng),
        PreparedColumnType::PhoneJa => random_phone(rng),
        PreparedColumnType::AddressJa => random_address(rng),
        PreparedColumnType::CompanyNameJa => random_company_name(rng),
        PreparedColumnType::Uuid => random_uuid(rng),
        PreparedColumnType::PrefectureJa => random_prefecture(rng),
        // context(前の列のprefecture_ja/name_ja)が無い状態での単独生成。
        // 文脈付きの生成はgenerate_cellが行う
        PreparedColumnType::CityJa => random_city(rng, None),
        PreparedColumnType::KatakanaName => random_katakana_name(rng, None),
        PreparedColumnType::Enum { choices } => choices[rng.gen_range(0..choices.len())].clone(),
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
        PreparedColumnType::NameJa
            | PreparedColumnType::Email
            | PreparedColumnType::Date { .. }
            | PreparedColumnType::PostalCode
            | PreparedColumnType::PhoneJa
            | PreparedColumnType::AddressJa
            | PreparedColumnType::CompanyNameJa
            | PreparedColumnType::Uuid
            | PreparedColumnType::PrefectureJa
            | PreparedColumnType::CityJa
            | PreparedColumnType::KatakanaName
            | PreparedColumnType::Enum { .. }
    )
}

// 進捗バーを表示する行数のしきい値。これより少ない行数だと一瞬で終わってしまい、
// バーを表示してもチラッと見えるだけで邪魔なうえ、cargo testの出力も汚れるので隠す
const PROGRESS_BAR_THRESHOLD: u32 = 1000;

fn new_progress_bar(row_count: u32) -> indicatif::ProgressBar {
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

// 全行・全列の値を、rayonで並列に生成する。CSV/SQL/JSON/Excelどの出力形式でも
// 「値を作る」部分は共通なので、ここに一本化している(進捗バーの更新もここでまとめて行う)。
// ファイルへの書き込み(直列処理が必要)は、それぞれのbuild_*関数が個別に行う。
fn generate_all_rows(row_count: u32, columns: &[PreparedColumn], base_seed: u64) -> Vec<Vec<Option<String>>> {
    let progress = new_progress_bar(row_count);

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

// SQLの中身(行データはgenerate_all_rowsで生成済みのものを受け取る)をUTF-8の文字列として
// メモリ上で組み立てる(ファイルにはまだ書かない)。複数形式を同時出力するとき、同じ行データを
// 形式の数だけ重複生成しないよう、「生成」(generate_all_rows)と「清書」(この関数)を分けている。
fn build_sql_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    table_name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let column_names = columns
        .iter()
        .map(|c| sql_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let table_ident = sql_ident(table_name);

    let value_rows: Vec<String> = rows
        .iter()
        .map(|row| {
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

    // INSERT文の組み立て(バッチ分割)はファイル1本を順番に書くだけなので並列化せず、直列に行う
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

// CSVの中身(行データは生成済みのものを受け取る)をUTF-8の文字列として組み立てる。
// NULL(None)はCSVでは空文字として書き出す
fn build_csv_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    writer.write_record(&headers)?; // ヘッダー行

    for row in rows {
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
    build_csv_from_rows(columns, &generate_all_rows(row_count, columns, base_seed))
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

fn write_text(text: &str, path: &str, encoding: Encoding) -> Result<(), Box<dyn std::error::Error>> {
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
fn build_json_from_rows(
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
) -> Result<String, Box<dyn std::error::Error>> {
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::with_capacity(columns.len());
            for (column, cell) in columns.iter().zip(row) {
                object.insert(column.name.clone(), cell_to_json(&column.kind, cell.as_deref()));
            }
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
    let Some(value) = cell else {
        return Ok(());
    };

    match kind {
        PreparedColumnType::Sequence | PreparedColumnType::Integer { .. } | PreparedColumnType::Float { .. } => {
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
fn write_xlsx_from_rows(
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
fn output_base_path(output: Option<&str>, format: Format, multiple_formats: bool) -> String {
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
fn write_output(
    format: Format,
    columns: &[PreparedColumn],
    rows: &[Vec<Option<String>>],
    table_name: Option<&str>,
    path: &str,
    encoding: Encoding,
) -> Result<(), Box<dyn std::error::Error>> {
    match format {
        Format::Csv => write_text(&build_csv_from_rows(columns, rows)?, path, encoding),
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
struct GeneratedTable<'a> {
    name: Option<&'a str>,
    columns: &'a [PreparedColumn],
    rows: &'a [Vec<Option<String>>],
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
// 戻り値は書き込んだ(パス, 行数)の一覧(mainが成功メッセージを1行ずつ表示するために使う)
fn write_output_multi_table(
    format: Format,
    tables: &[GeneratedTable],
    base_path: &str,
    encoding: Encoding,
) -> Result<Vec<(String, u32)>, Box<dyn std::error::Error>> {
    match format {
        Format::Csv | Format::Json => {
            // 書き込みを始める前に、サニタイズ後のファイル名が衝突しないか確認する
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
            for (table, path) in tables.iter().zip(paths) {
                let text = match format {
                    Format::Csv => build_csv_from_rows(table.columns, table.rows)?,
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

fn main() {
    let args = Args::parse();

    let schema_file = match load_schema(&args.config) {
        Ok(schema_file) => schema_file,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    // --format に同じ形式を重複指定されても1回だけ処理する(指定順は保つ)
    let mut formats: Vec<Format> = Vec::new();
    for &f in &args.format {
        if !formats.contains(&f) {
            formats.push(f);
        }
    }
    let multiple_formats = formats.len() > 1;

    if formats.contains(&Format::Xlsx) && matches!(args.encoding, Encoding::Sjis) {
        eprintln!("警告: --format xlsxでは--encodingは無視されます(Excelファイルは常にUTF-8相当の内部形式です)");
    }

    if !schema_file.multi_table {
        // 単一テーブル(schema.yamlにtables:が無い、これまで通りの形式)。
        // 既存の利用者への影響が絶対にないよう、以前と全く同じ手順・関数呼び出しのままにしてある。
        let schema = &schema_file.tables[0];

        let mut columns = match prepare_columns(schema) {
            Ok(columns) => columns,
            Err(e) => {
                eprintln!("エラーが発生しました: {}", e);
                return;
            }
        };

        let base_seed = args.seed.unwrap_or_else(rand::random);
        resolve_unique_pools(&mut columns, schema.row_count, base_seed);
        let rows = generate_all_rows(schema.row_count, &columns, base_seed);

        for format in formats {
            let path = output_base_path(args.output.as_deref(), format, multiple_formats);
            let result =
                write_output(format, &columns, &rows, schema.table_name.as_deref(), &path, args.encoding);

            match result {
                Ok(()) => println!(
                    "{}行のデータを {} ({}) に書き出しました",
                    schema.row_count, path, args.encoding
                ),
                Err(e) => eprintln!("エラーが発生しました: {}", e),
            }
        }
        return;
    }

    // 複数テーブル(tables:形式)。依存関係を解決してから、親→子の順に1テーブルずつ生成する
    let mut tables = match prepare_tables(&schema_file) {
        Ok(tables) => tables,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    let (deps, referenced) = match resolve_foreign_keys(&mut tables) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    let order = match topological_order(&deps, &tables) {
        Ok(order) => order,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    if let Err(e) = resolve_fk_reprs(&mut tables, &order) {
        eprintln!("エラーが発生しました: {}", e);
        return;
    }

    let base_seed = args.seed.unwrap_or_else(rand::random);
    let mut key_pools: HashMap<ColumnKey, Arc<Vec<String>>> = HashMap::new();
    let mut rows_by_table: Vec<Option<Vec<Vec<Option<String>>>>> =
        (0..tables.len()).map(|_| None).collect();

    for &i in &order {
        eprintln!("テーブル \"{}\" を生成中...", tables[i].name.as_deref().unwrap_or(""));

        if let Err(e) = fill_foreign_key_pools(&mut tables[i].columns, &key_pools) {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }

        // table_seedは宣言順index(i)を使う。トポロジカル順ではないので、
        // 単一テーブル(tables.len()==1)のときは常にbase_seedそのものになる
        let seed = table_seed(base_seed, i);
        let row_count = tables[i].row_count;
        resolve_unique_pools(&mut tables[i].columns, row_count, seed);
        let rows = generate_all_rows(row_count, &tables[i].columns, seed);

        if let Err(e) = collect_key_pools(&tables[i], &rows, &referenced, &mut key_pools) {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }

        rows_by_table[i] = Some(rows);
    }

    // 依存順(親が先)にGeneratedTableへまとめる(SQL1本出力のINSERT順のため)
    let generated: Vec<GeneratedTable> = order
        .iter()
        .map(|&i| GeneratedTable {
            name: tables[i].name.as_deref(),
            columns: &tables[i].columns,
            rows: rows_by_table[i].as_ref().unwrap(),
        })
        .collect();

    for format in formats {
        let base_path = output_base_path(args.output.as_deref(), format, multiple_formats);
        match write_output_multi_table(format, &generated, &base_path, args.encoding) {
            Ok(written) => {
                for (path, row_count) in written {
                    println!("{}行のデータを {} ({}) に書き出しました", row_count, path, args.encoding);
                }
            }
            Err(e) => eprintln!("エラーが発生しました: {}", e),
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

    fn schema_file_from_yaml(yaml: &str) -> Result<SchemaFile, Box<dyn std::error::Error>> {
        let raw: RawSchemaFile = serde_yaml::from_str(yaml).expect("テスト用YAMLのパースに失敗した");
        normalize_schema_file(raw)
    }

    // 複数テーブルschema.yamlから、依存関係解決・repr確定まで済んだPreparedTableを作る
    // (F4-3のテストで繰り返し使う準備処理をまとめたもの)
    fn prepared_tables_from_yaml(yaml: &str) -> Result<(Vec<PreparedTable>, Vec<usize>), Box<dyn std::error::Error>> {
        let file = schema_file_from_yaml(yaml)?;
        let mut tables = prepare_tables(&file)?;
        let (deps, _referenced) = resolve_foreign_keys(&mut tables)?;
        let order = topological_order(&deps, &tables)?;
        resolve_fk_reprs(&mut tables, &order)?;
        Ok((tables, order))
    }

    #[test]
    fn sql_literal_quotes_text_columns_and_escapes_quote() {
        let quoted = sql_literal(&PreparedColumnType::NameJa, "O'Brien");
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

    #[test]
    fn prepare_columns_rejects_duplicate_column_names() {
        let schema = schema_from_yaml(
            "row_count: 1\ncolumns:\n  - name: id\n    type: sequence\n  - name: id\n    type: email\n",
        );
        assert!(prepare_columns(&schema).is_err());
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

    #[test]
    fn prepare_columns_rejects_unique_on_unsupported_type() {
        let schema = schema_from_yaml(
            "row_count: 5\ncolumns:\n  - name: a\n    type: address_ja\n    unique: true\n",
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
        assert_eq!(build_csv_from_rows(&columns, &rows).unwrap(), build_csv(schema.row_count, &columns, 42).unwrap());
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
        let csv_text = build_csv_from_rows(&columns, &rows).unwrap();
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

    // --- ここから F4-3(テーブル間の外部キー整合性) のテスト ---

    // prepared_tables_from_yamlより先(依存関係解決の前段階)で必要になる、
    // 実際の値生成まで行うテスト用ヘルパー。main()の複数テーブル生成ループと同じ手順を踏む
    #[allow(clippy::type_complexity)] // テスト専用ヘルパーの戻り値。分割するほどの複雑さではない
    fn run_multi_table(
        yaml: &str,
        base_seed: u64,
    ) -> Result<(Vec<PreparedTable>, Vec<usize>, Vec<Vec<Vec<Option<String>>>>), Box<dyn std::error::Error>> {
        let file = schema_file_from_yaml(yaml)?;
        let mut tables = prepare_tables(&file)?;
        let (deps, referenced) = resolve_foreign_keys(&mut tables)?;
        let order = topological_order(&deps, &tables)?;
        resolve_fk_reprs(&mut tables, &order)?;

        let mut key_pools: HashMap<ColumnKey, Arc<Vec<String>>> = HashMap::new();
        let mut rows_by_table: Vec<Vec<Vec<Option<String>>>> = (0..tables.len()).map(|_| Vec::new()).collect();

        for &i in &order {
            fill_foreign_key_pools(&mut tables[i].columns, &key_pools)?;
            let seed = table_seed(base_seed, i);
            let row_count = tables[i].row_count;
            resolve_unique_pools(&mut tables[i].columns, row_count, seed);
            let rows = generate_all_rows(row_count, &tables[i].columns, seed);
            collect_key_pools(&tables[i], &rows, &referenced, &mut key_pools)?;
            rows_by_table[i] = rows;
        }

        Ok((tables, order, rows_by_table))
    }

    #[test]
    fn legacy_single_table_yaml_is_not_multi_table() {
        let file = schema_file_from_yaml(
            "row_count: 3\ncolumns:\n  - name: id\n    type: sequence\n",
        )
        .unwrap();
        assert!(!file.multi_table);
        assert_eq!(file.tables.len(), 1);
    }

    #[test]
    fn tables_format_is_recognized_as_multi_table() {
        let file = schema_file_from_yaml(
            "tables:\n  - name: users\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n  - name: orders\n    row_count: 2\n    columns:\n      - name: id\n        type: sequence\n",
        )
        .unwrap();
        assert!(file.multi_table);
        assert_eq!(file.tables.len(), 2);
        assert_eq!(file.tables[0].table_name.as_deref(), Some("users"));
        assert_eq!(file.tables[1].row_count, 2);
    }

    #[test]
    fn tables_and_top_level_columns_together_is_an_error() {
        let result = schema_file_from_yaml(
            "row_count: 3\ncolumns:\n  - name: id\n    type: sequence\ntables:\n  - name: users\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn empty_tables_list_is_an_error() {
        assert!(schema_file_from_yaml("tables: []\n").is_err());
    }

    #[test]
    fn table_without_name_is_an_error() {
        let result = schema_file_from_yaml(
            "tables:\n  - row_count: 3\n    columns:\n      - name: id\n        type: sequence\n",
        );
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_table_names_is_an_error() {
        let result = schema_file_from_yaml(
            "tables:\n  - name: users\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n  - name: users\n    row_count: 2\n    columns:\n      - name: id\n        type: sequence\n",
        );
        assert!(result.is_err());
    }

    const PARENT_CHILD_YAML: &str = "tables:\n  - name: users\n    row_count: 10\n    columns:\n      - name: id\n        type: sequence\n  - name: orders\n    row_count: 30\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n";

    #[test]
    fn foreign_key_values_are_all_members_of_the_parent_generated_values() {
        let (_tables, order, rows) = run_multi_table(PARENT_CHILD_YAML, 42).unwrap();
        // ordersはusersより後に生成される(依存関係上)
        let users_idx = order[0];
        let orders_idx = order[1];
        let user_ids: std::collections::HashSet<&str> =
            rows[users_idx].iter().map(|r| r[0].as_deref().unwrap()).collect();
        for row in &rows[orders_idx] {
            let referenced = row[1].as_deref().unwrap();
            assert!(user_ids.contains(referenced), "{referenced} not found in users.id");
        }
    }

    #[test]
    fn topological_order_puts_parent_before_child_even_when_declared_child_first() {
        // orders(子)をusers(親)より先に書いても、生成順は親が先になるはず
        let yaml = "tables:\n  - name: orders\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n  - name: users\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n";
        let (tables, order, _rows) = run_multi_table(yaml, 1).unwrap();
        let users_pos = order.iter().position(|&i| tables[i].name.as_deref() == Some("users")).unwrap();
        let orders_pos = order.iter().position(|&i| tables[i].name.as_deref() == Some("orders")).unwrap();
        assert!(users_pos < orders_pos);
    }

    #[test]
    fn self_reference_is_an_error() {
        let yaml = "tables:\n  - name: orders\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n      - name: parent_id\n        type: foreign_key\n        references: orders.id\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn cyclic_reference_is_an_error() {
        let yaml = "tables:\n  - name: a\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: b_id\n        type: foreign_key\n        references: b.id\n  - name: b\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: a_id\n        type: foreign_key\n        references: a.id\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn reference_to_unknown_table_is_an_error() {
        let yaml = "tables:\n  - name: orders\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn reference_to_unknown_column_is_an_error() {
        let yaml = "tables:\n  - name: users\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n  - name: orders\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.nope\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn foreign_key_column_rejects_unique() {
        let yaml = "tables:\n  - name: users\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n  - name: orders\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n        unique: true\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn referenced_parent_column_with_null_rate_is_an_error() {
        let yaml = "tables:\n  - name: users\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n        null_rate: 0.2\n  - name: orders\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n";
        assert!(prepared_tables_from_yaml(yaml).is_err());
    }

    #[test]
    fn empty_referenced_parent_table_is_an_error() {
        let yaml = "tables:\n  - name: users\n    row_count: 0\n    columns:\n      - name: id\n        type: sequence\n  - name: orders\n    row_count: 3\n    columns:\n      - name: id\n        type: sequence\n      - name: user_id\n        type: foreign_key\n        references: users.id\n";
        assert!(run_multi_table(yaml, 1).is_err());
    }

    #[test]
    fn diamond_dependency_generates_successfully_with_valid_references() {
        // d → b, d → c, b → a, c → a
        let yaml = "tables:\n  - name: a\n    row_count: 4\n    columns:\n      - name: id\n        type: sequence\n  - name: b\n    row_count: 6\n    columns:\n      - name: id\n        type: sequence\n      - name: a_id\n        type: foreign_key\n        references: a.id\n  - name: c\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n      - name: a_id\n        type: foreign_key\n        references: a.id\n  - name: d\n    row_count: 8\n    columns:\n      - name: id\n        type: sequence\n      - name: b_id\n        type: foreign_key\n        references: b.id\n      - name: c_id\n        type: foreign_key\n        references: c.id\n";
        let (tables, order, rows) = run_multi_table(yaml, 3).unwrap();
        assert_eq!(order.len(), 4);

        let idx_of = |name: &str| tables.iter().position(|t| t.name.as_deref() == Some(name)).unwrap();
        let a_ids: std::collections::HashSet<&str> =
            rows[idx_of("a")].iter().map(|r| r[0].as_deref().unwrap()).collect();
        for row in &rows[idx_of("b")] {
            assert!(a_ids.contains(row[1].as_deref().unwrap()));
        }
        for row in &rows[idx_of("c")] {
            assert!(a_ids.contains(row[1].as_deref().unwrap()));
        }
    }

    #[test]
    fn multi_hop_foreign_key_values_are_transitively_contained_and_repr_propagates() {
        // c.b_id → b.a_id → a.id (aはsequenceなので、cのSQL上の値は無クォートになるはず)
        let yaml = "tables:\n  - name: a\n    row_count: 5\n    columns:\n      - name: id\n        type: sequence\n  - name: b\n    row_count: 8\n    columns:\n      - name: id\n        type: sequence\n      - name: a_id\n        type: foreign_key\n        references: a.id\n  - name: c\n    row_count: 12\n    columns:\n      - name: id\n        type: sequence\n      - name: b_id\n        type: foreign_key\n        references: b.a_id\n";
        let (tables, order, rows) = run_multi_table(yaml, 5).unwrap();
        let idx_of = |name: &str| tables.iter().position(|t| t.name.as_deref() == Some(name)).unwrap();

        let a_ids: std::collections::HashSet<&str> =
            rows[idx_of("a")].iter().map(|r| r[0].as_deref().unwrap()).collect();
        let b_a_ids: std::collections::HashSet<&str> =
            rows[idx_of("b")].iter().map(|r| r[1].as_deref().unwrap()).collect();

        for row in &rows[idx_of("c")] {
            let v = row[1].as_deref().unwrap();
            assert!(b_a_ids.contains(v));
            assert!(a_ids.contains(v));
        }

        // reprがsequence(Integer)まで伝播していること = SQLでクォートされないこと
        let c_col = tables[idx_of("c")].columns.iter().find(|col| col.name == "b_id").unwrap();
        assert!(!is_text_column(&c_col.kind));

        let _ = order;
    }

    #[test]
    fn build_sql_multi_emits_parent_insert_before_child_insert() {
        let (tables, order, rows) = run_multi_table(PARENT_CHILD_YAML, 9).unwrap();
        let generated: Vec<GeneratedTable> = order
            .iter()
            .map(|&i| GeneratedTable { name: tables[i].name.as_deref(), columns: &tables[i].columns, rows: &rows[i] })
            .collect();
        let sql = build_sql_multi(&generated).unwrap();
        let users_pos = sql.find("INSERT INTO \"users\"").unwrap();
        let orders_pos = sql.find("INSERT INTO \"orders\"").unwrap();
        assert!(users_pos < orders_pos);
    }

    #[test]
    fn write_xlsx_tables_creates_one_sheet_per_table() {
        let (tables, order, rows) = run_multi_table(PARENT_CHILD_YAML, 11).unwrap();
        let generated: Vec<GeneratedTable> = order
            .iter()
            .map(|&i| GeneratedTable { name: tables[i].name.as_deref(), columns: &tables[i].columns, rows: &rows[i] })
            .collect();

        let path = std::env::temp_dir().join("dummy_data_gen_test_multi.xlsx");
        let path_str = path.to_str().unwrap();
        write_xlsx_tables(&generated, path_str, true).unwrap();

        use calamine::Reader;
        let workbook: calamine::Xlsx<_> = calamine::open_workbook(path_str).unwrap();
        assert_eq!(workbook.sheet_names(), vec!["users".to_string(), "orders".to_string()]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn same_seed_produces_identical_multi_table_output() {
        let (_t1, o1, r1) = run_multi_table(PARENT_CHILD_YAML, 123).unwrap();
        let (_t2, o2, r2) = run_multi_table(PARENT_CHILD_YAML, 123).unwrap();
        assert_eq!(o1, o2);
        assert_eq!(r1, r2);
    }

    #[test]
    fn tables_with_identical_columns_produce_different_data() {
        let yaml = "tables:\n  - name: t1\n    row_count: 10\n    columns:\n      - name: v\n        type: integer\n        min: 0\n        max: 1000000\n  - name: t2\n    row_count: 10\n    columns:\n      - name: v\n        type: integer\n        min: 0\n        max: 1000000\n";
        let (_tables, _order, rows) = run_multi_table(yaml, 77).unwrap();
        assert_ne!(rows[0], rows[1]);
    }

    #[test]
    fn table_seed_zero_index_matches_base_seed_exactly() {
        assert_eq!(table_seed(12345, 0), 12345);
    }

    #[test]
    fn table_file_path_inserts_table_name_before_extension() {
        assert_eq!(table_file_path("output.csv", "users"), "output_users.csv");
        assert_eq!(table_file_path("result.csv", "orders"), "result_orders.csv");
    }

    #[test]
    fn sanitize_file_component_replaces_unsafe_characters() {
        assert_eq!(sanitize_file_component("a/b:c"), "a_b_c");
        assert_eq!(sanitize_file_component("normal_name"), "normal_name");
    }

    #[test]
    fn write_output_multi_table_detects_filename_collision() {
        let (tables, order, rows) = run_multi_table(PARENT_CHILD_YAML, 13).unwrap();
        // 2つとも同じテーブル名(sanitize後に衝突)になるように、あえて同名のGeneratedTableを作る
        let generated: Vec<GeneratedTable> = order
            .iter()
            .map(|&i| GeneratedTable { name: Some("dup"), columns: &tables[i].columns, rows: &rows[i] })
            .collect();
        let result = write_output_multi_table(Format::Csv, &generated, "output.csv", Encoding::Utf8);
        assert!(result.is_err());
    }
}
