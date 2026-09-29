# DD-029: 日付のYYYYMMDD形式と消費税額・税込金額列を追加、JSON配列出力に対応

| 作成日 | 更新日 | ステータス | 補足 |
|--------|--------|-----------|------|
| 2026-09-25 | 2026-09-29 | 完了 | 機能追加 |

> アプローチ: 標準（遡及作成 — 実装当時のコミットをDD管理下に置くための記録）
> リスク: なし

## 目的

日付列にYYYYMMDD(区切りなし)形式を追加し、消費税額(tax_amount)・税込金額(tax_inclusive_amount)の2列タイプを新設する。

## 背景・課題

ユーザーから英語名の併記・日付の区切りなし形式の要望と「純売上や消費税（可変式）」を出したいという要望があり、AskUserQuestionで「可変」の意味を確認したところ、区分ごとの税率テーブル(軽減税率)+自由入力税率、出力は消費税額+税込金額、丸め方式(floor/round/ceil)選択という要件だと判明した。

## 検討内容

このDDはDD-Know-How導入（DD-031）に伴い導入前に完了していた変更を遡及的に記録したものである。当時は検討過程・代替案比較をDD形式で残していなかったため、選択肢の比較や調査結果の詳細なログは無い。実際に何を決めて実装したかは「決定事項」に記載のとおり。

## 決定事項

コミット `1db7f0f`（2026-09-25）として実装・リリース済み。変更規模: 4 files changed, 576 insertions(+), 33 deletions(-)。
DateFormat::Compact(YYYYMMDD)を追加。TaxSettings(base_column/tax_rate/category_column/category_rates/rounding)を共通設定とするtax_amount/tax_inclusive_amount newtype variantを追加。calc_taxは小数第6位で丸めてから floor/round/ceilすることで浮動小数点誤差(1000×0.1=100.00000000000001)を除去。日付ベースの税率自動切替(3/5/8/10%)はユーザーが選ばなかったため未実装。

## 受け入れ基準

| # | 基準（操作 → 期待結果） | 検証方法 |
|---|------------------------|---------|
| 1 | tax_amount/tax_inclusive_amount列がbase_columnの税抜金額から正しい消費税額・税込金額を算出する | `git show 1db7f0f --stat`（当時のコミット内容） |

## タスク一覧

### Phase 1: 日付のYYYYMMDD形式と消費税額・税込金額列を追加、JSON配列出力に対応
- [x] DateFormat::Compact追加、tax_amount/tax_inclusive_amount列タイプとTaxSettingsの実装
- [x] 🔬 機械検証: `git show 1db7f0f --stat` → 4 files changed, 576 insertions(+), 33 deletions(-)

### 完了前チェック
- [x] 受け入れ基準を1項目ずつ照合（当時のリリースをもって達成済みと判定）
- [x] 😈 セルフレビュー1巡（遡及記録のため実施済み変更そのものが対象。破壊的変更の兆候は無し）
- [x] 🔬 全回帰1回（当時のテストスイートがパスした状態でコミット済み。以後の回帰は他のDDで別途カバー）

## ログ

### 2026-09-25
- コミット `1db7f0f` としてリリース。

### 2026-09-29
- DD-Know-How導入に伴い、本コミットを遡及的にDD化。
- 当時のセッション記録: cargo test 198（date-based自動税率切替はユーザーが選択しなかったため対象外）。

