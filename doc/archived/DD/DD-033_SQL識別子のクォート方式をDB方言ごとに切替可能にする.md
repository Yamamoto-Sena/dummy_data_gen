# DD-033: SQL識別子のクォート方式をDB方言ごとに切替可能にする

| 作成日 | 更新日 | ステータス | 補足 |
|--------|--------|-----------|------|
| 2026-10-06 | 2026-10-06 | 完了 | SqlDialect(standard/mysql/postgresql/sqlserver/sqlite)をエンジン・CLI・GUI全面対応 |

> アプローチ: TDD（識別子クォートのロジックは正解が明確。GUIにドロップダウン追加があるため最終Phaseのみ画面エビデンスを取る）
> リスク: なし

## 目的

SQL出力(`--format sql`)の識別子(テーブル名・カラム名)を囲むクォート文字を、出力先のDB方言ごとに切り替えられるようにする。

## 背景・課題

現状`sql_ident`は常にダブルクォート(`"..."`)で囲む。これはPostgreSQL/標準SQL/SQLiteには有効だが、MySQLのデフォルト設定(`ANSI_QUOTES`無効時)ではダブルクォートは文字列リテラルとして解釈され、識別子として使えない。ユーザーがMySQLにそのままINSERT文を流し込めなかったのはこれが原因。他のSQL(SQL Server等)にも対応できる形にしたい、という要望。

## 検討内容

ユーザーにAskUserQuestionで確認し、「クォート文字を直接指定」ではなく「DB方言を選んで内部でクォート文字を自動選択」方式を採用（DB方言の知識が無いユーザーにも分かりやすい）。

対応方言とクォート文字:

| 方言 | クォート文字 | エスケープ(自身が含まれる場合) |
|------|------------|------------------------------|
| 標準SQL / PostgreSQL / SQLite | `"識別子"` | `"` → `""`（既存動作そのまま） |
| MySQL | `` `識別子` `` | `` ` `` → ``` `` ``` |
| SQL Server | `[識別子]` | `]` → `]]` |

PostgreSQL・SQLiteは標準SQLと同じダブルクォートのため、実装上は`SqlDialect`を5バリアント(`Standard`/`Mysql`/`PostgreSql`/`SqlServer`/`Sqlite`)で持ち、クォート文字を返すメソッドで3パターンに畳み込む（GUIの選択肢ラベルとenumを1:1対応させ、将来方言ごとに別の挙動が要る場合の拡張も楽にする）。

既定値は`Standard`（既存のダブルクォート）固定とし、未指定時の出力は1バイトも変えない（後方互換）。

## 決定事項

- `dummy_data_gen::SqlDialect` enumを新設。`sql_ident`がこれを引数に取り、方言に応じたクォート文字でエスケープする。
- `build_sql_from_rows`/`write_sql_streaming`/`build_sql_multi`/`write_output`/`write_output_multi_table`のSQL分岐すべてに`dialect: SqlDialect`引数を追加（既定`Standard`）。
- CLI: `--sql-dialect`（`clap::ValueEnum`、既定`standard`）を追加。
- GUI: 単一テーブル`GenerateRequest`/複数テーブル`GenerateRequestMulti`(Tauri)・`GenerateRequestWeb`系(src-server)に`sql_dialect: Option<String>`(`#[serde(default)]`、Noneは`Standard`扱い)を追加。`ExportPanel.tsx`でSQL形式選択時のみ方言ドロップダウンを表示。

## 受け入れ基準

| # | 基準（操作 → 期待結果） | 検証方法 |
|---|------------------------|---------|
| 1 | `--sql-dialect mysql`でSQL出力 → 識別子がバッククォートで囲まれる | Phase 2 Rustテスト |
| 2 | `--sql-dialect sqlserver`でSQL出力 → 識別子が角カッコで囲まれる | Phase 2 Rustテスト |
| 3 | `--sql-dialect`省略 → 従来通りダブルクォート、既存出力から1バイトも変わらない | Phase 2 既存テスト全パス |
| 4 | `--sql-dialect postgresql`/`sqlite` → ダブルクォート(標準と同じ) | Phase 2 Rustテスト |
| 5 | 識別子に方言固有のクォート文字自身(`` ` ``/`]`/`"`)が含まれていても、二重化されて構文が壊れない | Phase 2 Rustテスト |
| 6 | GUIでSQL形式を選ぶと方言ドロップダウンが表示され、選んだ方言が生成結果(ダウンロードしたSQL)に反映される | Phase 4 ブラウザ確認+スクリーンショット |

## タスク一覧

### Phase 1: テスト設計
- [x] シナリオ洗い出し（添付 `DD-033/scenarios.md`）: 5方言×識別子エスケープ×単一テーブル/複数テーブル/ストリーミングの組み合わせから主要パターンを選定
- [x] 👀 ユーザーレビュー・合意後に次Phaseへ（GUI含む全範囲で進める方針に合意）

### Phase 2: Rust engine実装（TDD）
**Red:**
- [x] `dummy_data_gen/src/lib.rs`: `SqlDialect` enumのバリアント・クォート文字判定・エスケープのテストを追加（`sql_ident`単体 + `build_sql_from_rows`経由の既存テスト群への`dialect`引数追加）
- [x] テスト実行 → 新規分は失敗(Red)を確認

**Green:**
- [x] `SqlDialect` enum新設（`Clone`/`Copy`/`ValueEnum`/`PartialEq`/`Eq`/`Debug`）
- [x] `sql_ident(name: &str, dialect: SqlDialect) -> String` に書き換え
- [x] `build_sql_from_rows`/`build_sql`(test専用)/`write_sql_streaming`/`build_sql_multi`/`write_output`/`write_output_multi_table`のSQL分岐に`dialect`引数を追加・伝播
- [x] テスト実行 → 全件成功(Green)。`cargo test` 210→216(新規6件)

**Refactor:**
- [x] 既存コメント(`sql_ident`の説明)を方言対応に合わせて更新
- [x] 🔬 機械検証: `cargo test`（dummy_data_gen） → 216 passed / 0 failed

### Phase 3: CLI対応
- [x] `dummy_data_gen/src/main.rs`: `Args`に`--sql-dialect`（`clap::ValueEnum`、既定`standard`、`#[value(name=...)]`でkebab化を回避）を追加し、SQL出力呼び出しに渡す
- [x] `dummy_data_gen/README.md`/`CLAUDE.md`: `--sql-dialect`の説明を追加
- [x] 🔬 機械検証: `cargo run -- --format sql --sql-dialect mysql/sqlserver/(省略)` のスモーク実行 → バッククォート/角カッコ/ダブルクォートをそれぞれ確認

### Phase 4: GUI対応
- [x] `dummygen_jp_gui/src-tauri/src/lib.rs`: `GenerateRequest`/`GenerateRequestMulti`に`sql_dialect: Option<String>`追加、`parse_sql_dialect`ヘルパーで`SqlDialect`に変換して`write_sql_streaming`/`write_output_multi_table`に渡す
- [x] `dummygen_jp_gui/src-server/src/main.rs`: 同様に`GenerateRequestWeb`/`GenerateRequestMultiWeb`へ追加(同名の`parse_sql_dialect`ヘルパー)
- [x] `dummygen_jp_gui/src/types.ts`: `OutputSqlDialect`型・`SQL_DIALECTS`選択肢定数（value/label）を追加
- [x] `dummygen_jp_gui/src/ExportPanel.tsx`: SQL形式選択時のみ表示する方言ドロップダウンを追加
- [x] `dummygen_jp_gui/src/savedConfigs.ts`/`App.tsx`/`useDummyGen.ts`: `sqlDialect`状態を追加し保存・復元・generate/generateMulti呼び出しに反映
- [x] 📸 実装完了後キャプチャ: ドロップダウン表示箇所を赤枠ハイライト（`DD-033/dd033-after-sql-dialect-dropdown.png`）
- [x] 🔬 機械検証: `cargo check`(src-tauri) / `npx tsc --noEmit` → クリーン
- [x] ブラウザ確認: Vite devサーバーでSQL選択時にドロップダウン表示を確認。src-serverのHTTP API(`/api/generate`)にmysql/sqlserver/省略時の3パターンでPOSTし、バッククォート・角カッコ・ダブルクォートがそれぞれ出力に反映されることを確認

### 完了前チェック
- [x] 受け入れ基準を1項目ずつ照合
- [x] 😈 セルフレビュー1巡（既存の`write_output`等、dialect引数を取り忘れている呼び出し元が無いか）→ 全呼び出し元を`grep`で洗い出し済み、漏れ無し
- [x] 🔬 全回帰1回: `cargo test`（dummy_data_gen 216件・src-tauri 15件・src-server 10件）+ `npx tsc --noEmit` → 全てpass/クリーン

## ログ

### 2026-10-06
- DD作成。ユーザーからAskUserQuestionでDB方言選択方式を確認済み
- Phase1〜4完了。Rustエンジン(`SqlDialect`新設・`sql_ident`等6関数にdialect引数追加)→CLI(`--sql-dialect`)→GUI(Tauri/src-server/フロントエンド)の順に実装し、各段階でテスト・ブラウザ確認を実施
- `dummy_data_gen`の全呼び出し元(`build_sql_from_rows`/`write_sql_streaming`/`write_output`/`build_sql_multi`/`write_output_multi_table`、テスト含む)を`grep`で洗い出して漏れなく更新。既存テストは全て`SqlDialect::Standard`を渡すだけで後方互換を維持(出力1バイトも変わらないことを既存アサーションの継続passで確認)
- `dummygen_jp_gui/src-tauri`・`src-server`は既存の`encoding`/`format`文字列変換パターンに合わせ、`parse_sql_dialect`ヘルパー(2箇所に同内容、既存の重複許容パターンを踏襲)で"standard"/"mysql"/"postgresql"/"sqlserver"/"sqlite"をパースし、未知の値は明示的にエラーにする(テストで確認)
- ブラウザでの確認は、Vite dev(ポート1430)でSQL選択時のドロップダウン表示をスクリーンショット確認、src-server(ポート3000、releaseビルド)に対し`fetch`で`/api/generate`を直接叩いてmysql/sqlserver/省略時の3パターンの出力を確認(Tauri invoke自体はブラウザから検証できないため、この部分はRustテストが実際の担保)
