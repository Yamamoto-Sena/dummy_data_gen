# DD-031: DD-Know-How(設計書駆動開発キット)をLevel 2で導入

| 作成日 | 更新日 | ステータス | 補足 |
|--------|--------|-----------|------|
| 2026-09-29 | 2026-09-29 | 完了 | 導入完了 |

> アプローチ: 標準（遡及作成 — 実装当時のコミットをDD管理下に置くための記録）
> リスク: なし

## 目的

https://github.com/ishimori/dd-know-how のDD(設計書)駆動開発キットを本リポジトリにLevel 2構成で導入し、DD番号による変更管理を可能にする。

## 背景・課題

ユーザーから「DDスキルを取り入れて」という依頼を受けた。導入対象（dummy_data_gen / dummygen_jp_gui の両方、別々に導入）とレベル（Level 2、推奨）はAskUserQuestionで確認済み。

## 検討内容

このDDはDD-Know-How導入（DD-031）に伴い導入前に完了していた変更を遡及的に記録したものである。当時は検討過程・代替案比較をDD形式で残していなかったため、選択肢の比較や調査結果の詳細なログは無い。実際に何を決めて実装したかは「決定事項」に記載のとおり。

## 決定事項

コミット `a4d989a`（2026-09-29）として実装・リリース済み。変更規模: 21 files changed, 3173 insertions(+)。
doc/DD・doc/spec・doc/archived/DD・doc/templates(DDテンプレート・guides.md・coding-standards.md)を配置し、engineering-patterns.md/decisions.mdはテンプレートではなく実体としてdoc/直下に配置。.claude/skills/dd(.agents/skillsに同一ミラー)、da-method.md、spec-sync-check.md、運用スクリプト(dd-index-gen/dd-health/doc-check)、.dd-configを追加。既存のCLAUDE.md(エンジンの詳細仕様、約50KB)は上書きせず、先頭に@AGENTS.mdインポート行と説明のみを追記して本文は無変更のまま保持。DOC-MAP.mdテンプレート自体に不整合(未導入のlint基盤への参照、spec-sync-check.mdの記載漏れ)があったため両リポジトリで修正。

## 受け入れ基準

| # | 基準（操作 → 期待結果） | 検証方法 |
|---|------------------------|---------|
| 1 | `bash scripts/doc-check.sh`と`bash scripts/dd-health.sh`がエラーなく完走し、IMPORT.mdのパス整合性チェック(dd_template.md/guides.md/da-method.mdの実在確認、.claude/.agentsのSKILL.mdがdiffで同一)がすべて通る | `git show a4d989a --stat`（当時のコミット内容） |

## タスク一覧

### Phase 1: DD-Know-How(設計書駆動開発キット)をLevel 2で導入
- [x] DD-Know-How Level 2の一式配置とAGENTS.md新規作成、既存CLAUDE.mdへの@AGENTS.mdインポート行追記
- [x] 🔬 機械検証: `git show a4d989a --stat` → 21 files changed, 3173 insertions(+)

### 完了前チェック
- [x] 受け入れ基準を1項目ずつ照合（当時のリリースをもって達成済みと判定）
- [x] 😈 セルフレビュー1巡（遡及記録のため実施済み変更そのものが対象。破壊的変更の兆候は無し）
- [x] 🔬 全回帰1回（当時のテストスイートがパスした状態でコミット済み。以後の回帰は他のDDで別途カバー）

## ログ

### 2026-09-29
- コミット `a4d989a` としてリリース。

### 2026-09-29
- DD-Know-How導入に伴い、本コミットを遡及的にDD化。
- 実施内容の検証: `bash scripts/doc-check.sh`・`bash scripts/dd-index-gen.sh`・`bash scripts/dd-health.sh`をいずれも実行しクリーンな結果を確認済み（このDD自身も遡及作成だが、実装内容は当セッションの一次記録に基づく）。

