use clap::Parser;
use dummy_data_gen::*;
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

        // CSV単体・SQL単体のときだけ、全行をメモリに載せずに済むストリーミング書き込みを使う
        // (GUIが実際に使うのもこの2形式のみのため)。JSON/XLSXを含む場合や複数形式の
        // 同時出力の場合は、これまで通り全行をメモリに載せてから書き出す(差分最小の原則)。
        if formats.len() == 1 && matches!(formats[0], Format::Csv | Format::Sql) {
            let format = formats[0];
            let path = output_base_path(args.output.as_deref(), format, multiple_formats);
            let progress = new_progress_bar(schema.row_count);
            let progress_for_closure = progress.clone();
            let on_progress = move |done: u64, _total: u64| progress_for_closure.set_position(done);

            let result = match format {
                // write_bomはfalse: CLIの出力は今まで通りBOM無しのまま変えない
                Format::Csv => write_csv_streaming(
                    schema.row_count,
                    &columns,
                    base_seed,
                    &path,
                    args.encoding,
                    DEFAULT_CHUNK_SIZE,
                    false,
                    on_progress,
                ),
                Format::Sql => match schema.table_name.as_deref() {
                    Some(table_name) => write_sql_streaming(
                        schema.row_count,
                        &columns,
                        base_seed,
                        table_name,
                        &path,
                        args.encoding,
                        DEFAULT_CHUNK_SIZE,
                        on_progress,
                    ),
                    None => {
                        Err("SQL出力(--format sql)には、schema.yamlに table_name の指定が必要です".into())
                    }
                },
                _ => unreachable!("csv/sql以外はこの分岐に来ない"),
            };
            progress.finish_and_clear();

            match result {
                Ok(()) => println!(
                    "{}行のデータを {} ({}) に書き出しました",
                    schema.row_count, path, args.encoding
                ),
                Err(e) => eprintln!("エラーが発生しました: {}", e),
            }
            return;
        }

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
