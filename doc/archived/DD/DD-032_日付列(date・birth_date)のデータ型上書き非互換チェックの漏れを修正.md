# DD-032: 日付列(date・birth_date)のデータ型上書き非互換チェックの漏れを修正

| 作成日 | 更新日 | ステータス | 補足 |
|--------|--------|-----------|------|
| 2026-09-29 | 2026-09-29 | 完了 | 不具合修正 |

> アプローチ: バグ修正・ライトパス(画面表示への影響は警告メッセージの追加のみ、原因が明白でUTと実APIで完結するため)
> リスク: なし

## 概要

| Bug# | 概要 | 重要度 |
|------|------|--------|
| 1 | `date`/`birth_date`列に`data_type`(データの型の上書き指定)で`INTEGER`/`FLOAT`/`BOOLEAN`を指定しても、`incompatible_data_type_warnings`(DD-026で導入した「明らかに壊れる組み合わせ」の警告)が一切反応せず、警告なしでSQL構文エラー・JSON値消失が起きる状態だった | MEDIUM |

## 原因分析

`incompatible_data_type_warnings`(`src/lib.rs`)は、`is_obviously_non_numeric_type`という「常にテキストしか生成しない列タイプ」の一覧に載っている列だけを警告対象にしていた。`date`/`birth_date`はこの一覧に入っておらず(理由: `format: compact`のときは"20240315"のような数字だけの文字列になり、postal_code等と同じく「書式次第で数値になりうる」ため一律には除外できない、という過去の判断)、結果として**どの書式でも一切チェックされていなかった**。

実機で確認: 既定の`ymd`書式(例:"2024-01-05")の`date`列に`data_type: INTEGER`を指定してプレビューしても、警告欄には何も表示されない(他の列の警告は正しく表示される)。この書式は`-`を含むため`i64`へのパースは常に失敗し、実際にはJSONで値が`null`に消え、SQLでは`2024-01-05`が無効な数値リテラルとして不正な構文になる。

## 修正方針

`date`/`birth_date`は書式(`DateFormat`)によって結論が変わるため、一律の型一覧には入れず、`incompatible_data_type_warnings`内で個別に判定する分岐を追加した:
- `data_type`がBOOLEAN系に解決される場合 → どの書式でも日付文字列がtrue/falseになることはないため、常に警告。
- `data_type`がINTEGER/FLOAT系に解決される場合 → `compact`(数字だけ)以外の書式(`ymd`/`iso8601`/`slash`/`wareki`)は区切り文字・漢字を含み確実に壊れるため警告。`compact`は数字だけの文字列で実際に数値変換できてしまうため対象外(postal_code等と同じ扱い)。

## 対象ファイル

| ファイル | 変更内容 |
|---------|---------|
| `src/lib.rs` | `date_format_incompatible_with_numeric`ヘルパーを追加。`incompatible_data_type_warnings`を「型一覧によるフィルタ」から「型一覧 or date/birth_dateの書式別判定」に変更し、警告理由の文言も分岐に応じて出し分け |

## 受け入れ基準

| # | 基準（操作 → 期待結果） | 検証方法 |
|---|------------------------|---------|
| 1 | `date`列(既定のymd書式)に`data_type: INTEGER`を指定すると警告が1件出る | `cargo test warns_when_date_with_hyphen_format_data_type_is_forced_to_integer` |
| 2 | `date`列を`format: compact`にした上で`data_type: INTEGER`を指定すると警告は出ない(compactは数字だけの文字列のため) | `cargo test no_warning_when_compact_date_data_type_is_forced_to_integer` |
| 3 | `format: compact`でも`data_type: BOOLEAN`なら警告が出る(true/falseには絶対にならないため) | `cargo test warns_when_compact_date_data_type_is_forced_to_boolean` |
| 4 | `birth_date`列でも同じ判定が働く | `cargo test warns_when_birth_date_data_type_is_forced_to_float` |
| 5 | 実際のGUI(ブラウザプレビュー経由でsrc-server `/api/preview`)でも上記と同じ結果になる | 本DDのログ参照(fetch結果を直接確認済み) |

## タスク一覧

### Phase 1: 判定漏れの修正・テスト追加
- [x] `date_format_incompatible_with_numeric`ヘルパー追加、`incompatible_data_type_warnings`をdate/birth_date対応に変更
- [x] 同根パターンの横展開確認: `is_obviously_non_numeric_type`一覧に載っていない他の列タイプ(`fixed`/`pattern`/`enum`/`credit_card_expiry`等)は、値が利用者の自由入力または書式次第で数値になりうるため意図的に対象外(DD-026からの既存方針)と確認。追加の漏れなし
- [x] テスト追加: `warns_when_date_with_hyphen_format_data_type_is_forced_to_integer` / `no_warning_when_compact_date_data_type_is_forced_to_integer` / `warns_when_compact_date_data_type_is_forced_to_boolean` / `warns_when_birth_date_data_type_is_forced_to_float`
- [x] 🔬 機械検証: `cargo test` → 206/206 → 210/210(新規4件追加)、全パス

### 完了前チェック
- [x] 受け入れ基準を1項目ずつ照合(#1〜#5すべて確認済み)
- [x] 😈 セルフレビュー1巡: BOOLEAN分岐がcompact書式でも正しく警告することを確認(INTEGER/FLOATの判定だけ書式分岐にして、BOOLEANの判定を書式分岐の外に出し忘れる、という同種のミスが無いか再読)。問題なし
- [x] 🔬 全回帰1回: `cargo test`(dummy_data_gen) 210/210、`cargo check`(src-tauri・src-server) 両方クリーン、`npx tsc --noEmit`(dummygen_jp_gui) クリーン

## ログ

### 2026-09-29
- ユーザーから「壊れている場所を探してほしい」「明らかに対応できないデータの型変換は警告を出してほしい」という依頼を受け、DD-026で実装済みの`incompatible_data_type_warnings`を起点に調査。
- ブラウザプレビュー(`http://localhost:1430`、Vite dev server)で`name_ja`列に`data_type: INTEGER`を指定 → 想定通り警告表示を確認(既存機能は正常)。
- 続けて`date`列(既定ymd書式)に`data_type: INTEGER`を指定 → 警告が出ないことを発見(本Bug)。
- `src-server`をリビルド・再起動し、`fetch('/api/preview')`を直接叩いて3パターン(compact+INTEGER→警告なし、compact+BOOLEAN→警告あり、birth_date+FLOAT→警告あり)を確認。すべて設計通りの結果。
- `dummygen_jp_gui/src-tauri`・`src-server`の`cargo check`、`npx tsc --noEmit`も実施しクリーンを確認(型変更を伴わない修正のため影響なしを裏付け)。
- 変更は`dummy_data_gen`リポジトリのみ(GUI側のコード変更なし)。コミットは未実施(ユーザー指示があれば次回実施)。
