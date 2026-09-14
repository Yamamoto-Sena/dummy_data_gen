use chrono::Datelike;
use clap::{Parser, ValueEnum};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rayon::prelude::*;
use serde::Deserialize;

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

    /// 出力形式(csv / sql / json)。sqlの場合はschema.yamlに table_name の指定が必要
    #[arg(long, value_enum, default_value_t = Format::Csv)]
    format: Format,

    /// 乱数のシード値。指定すると、同じ設定なら毎回同じデータが再現される(テストや再実行に便利)。
    /// 省略した場合は毎回ランダムなシードを使う
    #[arg(long)]
    seed: Option<u64>,

    /// 出力先ファイルパス。省略時は形式に応じて output.csv / output.sql / output.json に保存する
    #[arg(long)]
    output: Option<String>,
}

// 行番号ごとに独立したRNGを作る。base_seedが同じなら常に同じ値になるため、
// 並列実行(rayon)でどのスレッドがどの行を処理しても結果が変わらない。
// SmallRng(暗号強度は無いが高速なPRNG)を使うのは、ダミーデータ生成に暗号学的な安全性は不要なため。
fn row_rng(base_seed: u64, row_num: u32) -> SmallRng {
    SmallRng::seed_from_u64(base_seed.wrapping_add(row_num as u64))
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    #[value(name = "csv")]
    Csv,
    #[value(name = "sql")]
    Sql,
    #[value(name = "json")]
    Json,
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Format::Csv => write!(f, "csv"),
            Format::Sql => write!(f, "sql"),
            Format::Json => write!(f, "json"),
        }
    }
}

// schema.yaml の中身をそのまま受け止める箱(struct)。serdeが自動でYAML→structに変換する
#[derive(Deserialize)]
struct Schema {
    row_count: u32,
    // SQL出力(--format sql)のときだけ使う。CSV出力では不要なのでOptionにしている
    #[serde(default)]
    table_name: Option<String>,
    columns: Vec<ColumnDef>,
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
    Enum {
        choices: Vec<String>,
    },
}

fn default_decimals() -> u32 {
    2
}

fn load_schema(path: &str) -> Result<Schema, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("スキーマファイル({})の読み込みに失敗しました: {}", path, e))?;
    let schema: Schema = serde_yaml::from_str(&text)?;
    Ok(schema)
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
    Enum { choices: Vec<String> },
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
                ColumnType::Enum { choices } => {
                    if choices.is_empty() {
                        return Err(format!("列 \"{}\": choices には1つ以上の選択肢が必要です", c.name).into());
                    }
                    PreparedColumnType::Enum { choices: choices.clone() }
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
const PREFECTURES: &[&str] = &["東京都", "大阪府", "愛知県", "北海道", "福岡県"];
const CITIES: &[&str] = &["中央区", "港区", "西区", "本町", "緑区"];
const PHONE_PREFIXES: &[&str] = &["090", "080", "070"];

fn random_name(rng: &mut impl Rng) -> String {
    let last = LAST_NAMES[rng.gen_range(0..LAST_NAMES.len())];
    let first = FIRST_NAMES[rng.gen_range(0..FIRST_NAMES.len())];
    format!("{}{}", last, first)
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
    let pref = PREFECTURES[rng.gen_range(0..PREFECTURES.len())];
    let city = CITIES[rng.gen_range(0..CITIES.len())];
    format!("{}{}{}-{}", pref, city, rng.gen_range(1..20), rng.gen_range(1..20))
}

// 1列分の値を作る。null_rateの確率でNone(NULL)を返す
fn generate_cell(column: &PreparedColumn, row_num: u32, rng: &mut impl Rng) -> Option<String> {
    // uniqueな列は、あらかじめ用意しておいたプールからこの行番号に対応する値を取り出すだけ
    // (unique同士でnull_rateとの併用はprepare_columnsで禁止しているので、Noneになることは無い)
    if let Some(pool) = &column.unique_pool {
        return Some(pool[(row_num - 1) as usize].clone());
    }

    if column.null_rate > 0.0 && rng.gen_bool(column.null_rate) {
        None
    } else {
        Some(generate_value(&column.kind, row_num, rng))
    }
}

// 1行分(全列)の値を作る
fn generate_row(columns: &[PreparedColumn], row_num: u32, rng: &mut impl Rng) -> Vec<Option<String>> {
    columns.iter().map(|c| generate_cell(c, row_num, rng)).collect()
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
        PreparedColumnType::Enum { choices } => choices[rng.gen_range(0..choices.len())].clone(),
    }
}

// SQLのVALUES句に書くとき、文字列として ' ' で囲む必要がある列タイプかどうか
fn is_text_column(kind: &PreparedColumnType) -> bool {
    matches!(
        kind,
        PreparedColumnType::NameJa
            | PreparedColumnType::Email
            | PreparedColumnType::Date { .. }
            | PreparedColumnType::PostalCode
            | PreparedColumnType::PhoneJa
            | PreparedColumnType::AddressJa
            | PreparedColumnType::Enum { .. }
    )
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

// SQLの中身をまずUTF-8の文字列としてメモリ上で組み立てる(ファイルにはまだ書かない)
fn build_sql(
    row_count: u32,
    columns: &[PreparedColumn],
    table_name: &str,
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let column_names = columns
        .iter()
        .map(|c| sql_ident(&c.name))
        .collect::<Vec<_>>()
        .join(", ");
    let table_ident = sql_ident(table_name);

    // 各行の "(値1, 値2, ...)" 文字列を、行ごとに独立してrayonで並列に作る。
    // into_par_iter().map(...).collect() は行番号順を保ったまま結果を集めてくれる。
    let value_rows: Vec<String> = (1..=row_count)
        .into_par_iter()
        .map(|row_num| {
            let mut rng = row_rng(base_seed, row_num);
            let values: Vec<String> = generate_row(columns, row_num, &mut rng)
                .into_iter()
                .zip(columns)
                .map(|(cell, c)| match cell {
                    Some(v) => sql_literal(&c.kind, &v),
                    None => "NULL".to_string(), // SQLのNULLはクォートしてはいけない
                })
                .collect();
            format!("({})", values.join(", "))
        })
        .collect();

    // INSERT文の組み立て(バッチ分割)はファイル1本を順番に書くだけなので並列化せず、ここだけ直列に行う
    let mut sql = String::new();
    for batch in value_rows.chunks(SQL_BATCH_SIZE as usize) {
        sql.push_str(&format!("INSERT INTO {} ({}) VALUES\n", table_ident, column_names));
        sql.push_str(&batch.join(",\n"));
        sql.push_str(";\n\n");
    }

    Ok(sql)
}

// CSVの中身をまずUTF-8の文字列としてメモリ上で組み立てる(ファイルにはまだ書かない)
fn build_csv(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    // 行ごとの値生成はCPUを使う処理なので、rayonで複数スレッドに分散して並列に行う。
    // csv::Writerへの書き込みは順番が大事なので、生成が終わった後にまとめて直列に書く。
    // NULL(None)はCSVでは空文字として書き出す
    let rows: Vec<Vec<String>> = (1..=row_count)
        .into_par_iter()
        .map(|row_num| {
            let mut rng = row_rng(base_seed, row_num);
            generate_row(columns, row_num, &mut rng)
                .into_iter()
                .map(|cell| cell.unwrap_or_default())
                .collect()
        })
        .collect();

    let mut writer = csv::Writer::from_writer(Vec::new());
    let headers: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    writer.write_record(&headers)?; // ヘッダー行

    for row in &rows {
        writer.write_record(row)?;
    }

    let bytes = writer.into_inner()?; // 内部バッファ(UTF-8のバイト列)を取り出す
    Ok(String::from_utf8(bytes)?)
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

fn write_csv(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
    path: &str,
    encoding: Encoding,
) -> Result<(), Box<dyn std::error::Error>> {
    let csv_text = build_csv(row_count, columns, base_seed)?;
    write_text(&csv_text, path, encoding)
}

fn write_sql(
    row_count: u32,
    columns: &[PreparedColumn],
    table_name: &str,
    base_seed: u64,
    path: &str,
    encoding: Encoding,
) -> Result<(), Box<dyn std::error::Error>> {
    let sql_text = build_sql(row_count, columns, table_name, base_seed)?;
    write_text(&sql_text, path, encoding)
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
        _ => serde_json::Value::String(value.to_string()),
    }
}

// JSON(NDJSON = 1行1件のJSONオブジェクト)の中身をUTF-8の文字列としてメモリ上で組み立てる
fn build_json(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
) -> Result<String, Box<dyn std::error::Error>> {
    let lines: Vec<String> = (1..=row_count)
        .into_par_iter()
        .map(|row_num| {
            let mut rng = row_rng(base_seed, row_num);
            let cells = generate_row(columns, row_num, &mut rng);
            let mut object = serde_json::Map::with_capacity(columns.len());
            for (column, cell) in columns.iter().zip(cells) {
                object.insert(column.name.clone(), cell_to_json(&column.kind, cell.as_deref()));
            }
            serde_json::to_string(&object).expect("serde_jsonのオブジェクト直列化は失敗しない")
        })
        .collect();

    let mut text = lines.join("\n");
    text.push('\n'); // CSV/SQL出力と同様、ファイル末尾に改行を入れておく
    Ok(text)
}

fn write_json(
    row_count: u32,
    columns: &[PreparedColumn],
    base_seed: u64,
    path: &str,
    encoding: Encoding,
) -> Result<(), Box<dyn std::error::Error>> {
    let json_text = build_json(row_count, columns, base_seed)?;
    write_text(&json_text, path, encoding)
}

fn main() {
    let args = Args::parse();

    let schema = match load_schema(&args.config) {
        Ok(schema) => schema,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    let mut columns = match prepare_columns(&schema) {
        Ok(columns) => columns,
        Err(e) => {
            eprintln!("エラーが発生しました: {}", e);
            return;
        }
    };

    // --seedが指定されていればそれを使い、無ければ実行のたびに変わるランダムな値を使う
    let base_seed = args.seed.unwrap_or_else(rand::random);

    // unique指定のある列の値プールは、base_seedが決まった後でないと計算できない
    resolve_unique_pools(&mut columns, schema.row_count, base_seed);

    let default_path = match args.format {
        Format::Csv => "output.csv",
        Format::Sql => "output.sql",
        Format::Json => "output.json",
    };
    let path = args.output.as_deref().unwrap_or(default_path);

    let result = match args.format {
        Format::Csv => write_csv(schema.row_count, &columns, base_seed, path, args.encoding),
        Format::Sql => match &schema.table_name {
            Some(table_name) => {
                write_sql(schema.row_count, &columns, table_name, base_seed, path, args.encoding)
            }
            None => Err("SQL出力(--format sql)には、schema.yamlに table_name の指定が必要です".into()),
        },
        Format::Json => write_json(schema.row_count, &columns, base_seed, path, args.encoding),
    };

    match result {
        Ok(()) => println!(
            "{}行のデータを {} ({}) に書き出しました",
            schema.row_count, path, args.encoding
        ),
        Err(e) => eprintln!("エラーが発生しました: {}", e),
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
}
