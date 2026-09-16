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

// 行番号ごとに独立したRNGを作る。base_seedが同じなら常に同じ値になるため、
// 並列実行(rayon)でどのスレッドがどの行を処理しても結果が変わらない。
// SmallRng(暗号強度は無いが高速なPRNG)を使うのは、ダミーデータ生成に暗号学的な安全性は不要なため。
pub fn row_rng(base_seed: u64, row_num: u32) -> SmallRng {
    SmallRng::seed_from_u64(base_seed.wrapping_add(row_num as u64))
}

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
// Serializeも付けているのは、GUI(dummygen_jp_gui)の「列設定をYAMLとして書き出す」機能
// (schema_file_to_yaml)のため。読み込み(Deserialize)は元々のload_schema用
#[derive(Deserialize, Serialize)]
pub struct Schema {
    pub row_count: u32,
    // SQL出力(--format sql)のときや、tables:形式でのテーブル名として使う
    #[serde(default, alias = "name", skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    pub columns: Vec<ColumnDef>,
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
pub struct SchemaFile {
    pub tables: Vec<Schema>,
    pub multi_table: bool,
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

#[derive(Deserialize, Serialize)]
pub struct ColumnDef {
    pub name: String,
    // 0.0〜1.0の確率でNULL(空)を混ぜる。省略時はNULLを混ぜない(0.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub null_rate: Option<f64>,
    // trueにすると、この列の値が行間で重複しないようにする。省略時はfalse
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<bool>,
    // flattenにより、typeやmin/maxなどの追加情報を「nameと同じ階層」から直接読み取れる
    #[serde(flatten)]
    pub column_type: ColumnType,
}

// tag = "type" にすると、YAML上の "type:" の値でどのバリアント(列タイプ)かを判定し、
// min/maxなどの残りのフィールドをそのバリアントの中身として読み取ってくれる
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
    KatakanaName,
    // katakana_nameの半角カタカナ版。直前のname_ja列を参照する挙動は同じ
    KatakanaNameHankaku,
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
    KatakanaName,
    KatakanaNameHankaku,
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

fn compile_pattern(pattern: &str) -> Result<Vec<PatternPiece>, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut pieces = Vec::new();
    let mut i = 0;

    // 直前に確定した「1文字ぶんの候補一覧」を、量指定子が来るまで一時的に持っておく
    let mut pending: Option<Vec<char>> = None;

    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 1;
                let escaped = *chars.get(i).ok_or_else(|| "パターンの末尾が\\で終わっている".to_string())?;
                flush_default(&mut pending, &mut pieces);
                pending = Some(vec![escaped]);
                i += 1;
            }
            '[' => {
                i += 1;
                flush_default(&mut pending, &mut pieces);
                let negate = chars.get(i) == Some(&'^');
                if negate {
                    i += 1;
                }
                let mut set = std::collections::BTreeSet::new();
                while chars.get(i) != Some(&']') {
                    let start = *chars.get(i).ok_or_else(|| "文字クラス[...]が]で閉じられていない".to_string())?;
                    if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|c| *c != ']') {
                        let end = chars[i + 2];
                        if start > end {
                            return Err(format!("文字クラスの範囲が逆順になっている: {start}-{end}"));
                        }
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

pub fn prepare_columns(schema: &Schema) -> Result<Vec<PreparedColumn>, Box<dyn std::error::Error>> {
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
                ColumnType::KatakanaName => PreparedColumnType::KatakanaName,
                ColumnType::KatakanaNameHankaku => PreparedColumnType::KatakanaNameHankaku,
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
                        if w.iter().any(|&v| v < 0.0) {
                            return Err(format!("列 \"{}\": weights に負の数は指定できません", c.name).into());
                        }
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
                    UniqueCapacity::Unsupported => {
                        return Err(format!(
                            "列 \"{}\": このtypeはuniqueに対応していません(enum/boolean/gender/blood_type/integer/date/name_ja/last_name_ja/first_name_ja/phone_ja/phone_ja_landlineのみ対応)",
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
                    UniqueCapacity::Enumerable(capacity) | UniqueCapacity::Retry(capacity)
                        if capacity < schema.row_count as u128 =>
                    {
                        return Err(format!(
                            "列 \"{}\": unique: 値の組み合わせが{}通りしかなく、row_count({})分のユニークな値を用意できません",
                            c.name, capacity, schema.row_count
                        )
                        .into());
                    }
                    UniqueCapacity::Enumerable(_) | UniqueCapacity::Retry(_) => {}
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
// katakana_name/katakana_name_hankaku/romaji_nameはいずれも「自分より前にあるname_ja列」
// しか参照しない設計なので、3つまとめて同じ警告ロジックで扱う
fn misplaced_katakana_name_warnings(columns: &[PreparedColumn]) -> Vec<String> {
    fn type_label(kind: &PreparedColumnType) -> &'static str {
        match kind {
            PreparedColumnType::KatakanaName => "katakana_name",
            PreparedColumnType::KatakanaNameHankaku => "katakana_name_hankaku",
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
                PreparedColumnType::KatakanaName
                    | PreparedColumnType::KatakanaNameHankaku
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

// unique制約を付けられる列タイプが取りうる値の組み合わせ数。
// postal_code/address_ja/floatは組み合わせが不連続・計算しづらいため非対応のまま
// (これらの値の重複を避けたい場合は、より小さい組み合わせ数のenum/integerで代用することを想定している)。
fn unique_capacity(kind: &PreparedColumnType) -> UniqueCapacity {
    match kind {
        PreparedColumnType::Boolean => UniqueCapacity::Enumerable(2),
        PreparedColumnType::Gender => UniqueCapacity::Enumerable(2),
        PreparedColumnType::BloodType => UniqueCapacity::Enumerable(4),
        PreparedColumnType::Integer { min, max } => {
            UniqueCapacity::Enumerable((*max as i128 - *min as i128 + 1) as u128)
        }
        PreparedColumnType::Date { span_days, .. } => UniqueCapacity::Enumerable(*span_days as u128 + 1),
        // unique:trueのときは(重み付けの有無にかかわらず)全選択肢を重複なく列挙するので、
        // weightsは意味を持たない(README/CLAUDE.mdに明記。エラーにはせず単に無視する)
        PreparedColumnType::Enum { choices, .. } => UniqueCapacity::Enumerable(choices.len() as u128),
        PreparedColumnType::NameJa { .. } => {
            UniqueCapacity::Enumerable((LAST_NAMES.len() * FIRST_NAMES.len()) as u128)
        }
        PreparedColumnType::LastNameJa => UniqueCapacity::Enumerable(LAST_NAMES.len() as u128),
        PreparedColumnType::FirstNameJa => UniqueCapacity::Enumerable(FIRST_NAMES.len() as u128),
        PreparedColumnType::RomajiName => {
            UniqueCapacity::Enumerable((LAST_NAMES_ROMAJI.len() * FIRST_NAMES_ROMAJI.len()) as u128)
        }
        PreparedColumnType::KatakanaLastName => UniqueCapacity::Enumerable(LAST_NAMES_KANA.len() as u128),
        PreparedColumnType::KatakanaFirstName => UniqueCapacity::Enumerable(FIRST_NAMES_KANA.len() as u128),
        // 携帯電話・固定電話は組み合わせ数(市外局番の数 × 10^8)が膨大でEnumerable方式では
        // 列挙しきれないが、実務で指定されるrow_count(最大100万)に対しては十分すぎるほど
        // 大きいため、Retry方式(値を作って重複チェック)で対応する
        PreparedColumnType::PhoneJa => UniqueCapacity::Retry(PHONE_PREFIXES.len() as u128 * 100_000_000),
        PreparedColumnType::PhoneJaLandline => {
            UniqueCapacity::Retry(PHONE_PREFIXES_LANDLINE.len() as u128 * 100_000_000)
        }
        _ => UniqueCapacity::Unsupported,
    }
}

// UniqueCapacity::Enumerableがこれを超える場合はエラーにする。組み合わせ全部をVecに
// 列挙するので、メモリを使いすぎない(や、あまりに時間がかかりすぎない)ようにするための安全弁。
// Retry方式は組み合わせを列挙しないため、この上限の対象外
const UNIQUE_CAPACITY_CAP: u128 = 2_000_000;

// UniqueCapacity::Enumerableで数えた組み合わせを、実際の文字列としてすべて列挙する
fn enumerate_values(kind: &PreparedColumnType) -> Vec<String> {
    match kind {
        PreparedColumnType::Boolean => vec!["true".to_string(), "false".to_string()],
        PreparedColumnType::Gender => vec!["男性".to_string(), "女性".to_string()],
        PreparedColumnType::BloodType => {
            vec!["A型".to_string(), "O型".to_string(), "B型".to_string(), "AB型".to_string()]
        }
        PreparedColumnType::Integer { min, max } => (*min..=*max).map(|v| v.to_string()).collect(),
        PreparedColumnType::Date { start_days, span_days, format } => (0..=*span_days)
            .map(|offset| {
                format_date(
                    chrono::NaiveDate::from_num_days_from_ce_opt(start_days + offset as i32)
                        .expect("span_daysの範囲内なので必ず有効な日付になる"),
                    *format,
                )
            })
            .collect(),
        PreparedColumnType::Enum { choices, .. } => choices.clone(),
        PreparedColumnType::NameJa { with_space } => (0..LAST_NAMES.len())
            .flat_map(|last_idx| {
                (0..FIRST_NAMES.len()).map(move |first_idx| format_name(last_idx, first_idx, *with_space))
            })
            .collect(),
        PreparedColumnType::LastNameJa => LAST_NAMES.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::FirstNameJa => FIRST_NAMES.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::RomajiName => (0..LAST_NAMES_ROMAJI.len())
            .flat_map(|last_idx| {
                (0..FIRST_NAMES_ROMAJI.len())
                    .map(move |first_idx| format!("{} {}", LAST_NAMES_ROMAJI[last_idx], FIRST_NAMES_ROMAJI[first_idx]))
            })
            .collect(),
        PreparedColumnType::KatakanaLastName => LAST_NAMES_KANA.iter().map(|s| s.to_string()).collect(),
        PreparedColumnType::KatakanaFirstName => FIRST_NAMES_KANA.iter().map(|s| s.to_string()).collect(),
        _ => unreachable!("UniqueCapacity::Enumerable以外の型はここに来ない(prepare_columnsで弾いている)"),
    }
}

// enumerate方式が使えない(組み合わせ数が膨大な)型向け。値を作っては既出かどうかを
// HashSetでチェックし、被っていたら作り直す方式でrow_count件のユニークな値を集める。
// prepare_columnsで「組み合わせ数 >= row_count」を事前に検証済みであり、かつ対象型は
// 組み合わせ数がrow_count(最大100万)よりずっと多いため、衝突は実務上まれで高速に集まる。
// 試行回数の上限は「実装のバグ等で無限ループにならない」ための安全弁であり、
// 事前検証を正しく通過している限り実際に到達することはない
fn build_unique_pool_by_retry(kind: &PreparedColumnType, row_count: u32, base_seed: u64, column_salt: u64) -> Vec<String> {
    let mut rng = SmallRng::seed_from_u64(base_seed.wrapping_add(column_salt));
    let mut seen = std::collections::HashSet::with_capacity(row_count as usize);
    let mut values = Vec::with_capacity(row_count as usize);
    let max_attempts = (row_count as u64).saturating_mul(1000).max(100_000);

    for _ in 0..max_attempts {
        if values.len() == row_count as usize {
            break;
        }
        let candidate = generate_value(kind, 0, &mut rng);
        if seen.insert(candidate.clone()) {
            values.push(candidate);
        }
    }

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
    if matches!(unique_capacity(kind), UniqueCapacity::Retry(_)) {
        return build_unique_pool_by_retry(kind, row_count, base_seed, column_salt);
    }
    let mut values = enumerate_values(kind);
    let mut rng = SmallRng::seed_from_u64(base_seed.wrapping_add(column_salt));
    values.shuffle(&mut rng);
    values.truncate(row_count as usize);
    values
}

// unique指定のある列すべてに対して、実際の値のプールを計算してPreparedColumnに詰める。
// base_seedが決まった後(=prepare_columnsの後)でないと呼べない
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

// 全FK列の参照先(テーブル/列の存在)を検証し、依存辺(deps[子テーブルindex] = 親テーブルindexのVec)と、
// 「後でプールを取り出す必要がある親の列」(referenced: (テーブル名, 列名) → 参照元の説明)を作る。
#[allow(clippy::type_complexity)] // (Vec<Vec<usize>>, HashMap<...>) は内部専用の戻り値で、これ以上分ける必要は薄い
pub fn resolve_foreign_keys(
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
pub fn topological_order(
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
pub fn resolve_fk_reprs(
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
pub fn table_seed(base_seed: u64, table_index: usize) -> u64 {
    base_seed.wrapping_add((table_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

// 親テーブル生成後、参照されている列の値をプール化する
pub fn collect_key_pools(
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
pub fn fill_foreign_key_pools(
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

// 複数テーブルをトポロジカル順(親→子)に1テーブルずつ生成する。
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
// Noneなら独自にランダムな氏名の読みを作る(katakana_name単独使用時のフォールバック)
fn random_katakana_name(rng: &mut impl Rng, context: Option<(usize, usize)>) -> String {
    let (last_idx, first_idx) = context.unwrap_or_else(|| random_name_indices(rng));
    format!("{}{}", LAST_NAMES_KANA[last_idx], FIRST_NAMES_KANA[first_idx])
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
        PreparedColumnType::KatakanaNameHankaku => {
            (Some(to_hankaku_katakana(&random_katakana_name(rng, ctx.last_name_indices))), None)
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
    let mut ctx = RowContext::default();
    let mut values = Vec::with_capacity(columns.len());

    for column in columns {
        let (cell, name_indices) = generate_cell(column, row_num, rng, &ctx);
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
        PreparedColumnType::KatakanaName => random_katakana_name(rng, None),
        PreparedColumnType::KatakanaNameHankaku => to_hankaku_katakana(&random_katakana_name(rng, None)),
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
            | PreparedColumnType::KatakanaName
            | PreparedColumnType::KatakanaNameHankaku
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
    let quote_style = if quote_all { csv::QuoteStyle::Always } else { csv::QuoteStyle::Necessary };
    let mut writer = csv::WriterBuilder::new().quote_style(quote_style).from_writer(Vec::new());
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
    build_csv_from_rows(columns, &generate_all_rows(row_count, columns, base_seed), false)
}

// build_csv_from_rowsとの違いは、全行をメモリに載せてから一括で書き出すのではなく、
// chunk_size行ずつ「生成してすぐファイルに書き足す」を繰り返す点(2節: ストリーミング化)。
// 大量行(最大100万行を想定)でも、メモリ上に保持するのは常に1チャンク分だけになる。
// on_progressはチャンクを書き終えるたびに(完了行数, 全行数)で呼ばれる。
// CLI側はindicatif::ProgressBar::set_positionを、GUI側はTauriのイベント発火を
// 渡すことを想定している。
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
    let file = std::fs::File::create(path)?;
    let mut out = std::io::BufWriter::new(file);
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

    for chunk_start in (1..=row_count).step_by(chunk_size as usize) {
        let chunk_end = (chunk_start + chunk_size - 1).min(row_count);
        let rows = generate_rows_range(columns, base_seed, chunk_start, chunk_end);

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
        write_chunk_text(&mut out, &text, encoding, &mut had_sjis_errors)?;

        done += (chunk_end - chunk_start + 1) as u64;
        on_progress(done, total);
    }

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
        let schema = schema_from_yaml("row_count: 0\ncolumns:\n  - name: id\n    type: sequence\n");
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
