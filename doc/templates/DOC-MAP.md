# DOC-MAP: ドキュメントインデックス

> doc/ フォルダ内の全ドキュメントの場所と目的。
> **ドキュメントの追加・移動時はここも更新すること。**

## プロジェクト概要

| ファイル | 説明 |
|---------|------|
| `doc/project-overview.md` | プロジェクトスコープ・環境・方針（未作成。最初に書くことを推奨） |

## プロセス・開発規約

| ファイル | 説明 |
|---------|------|
| `doc/da-method.md` | DA深掘り手法（参考文書 — 設計の岐路・レビュー指示時に使う） |
| `doc/engineering-patterns.md` | エンジニアリングパターン集（gotcha・定石。DAの同根パターンの昇格先） |
| `doc/decisions.md` | 意思決定記録（長寿命のアーキテクチャ決定。「なぜこうなってる？」の逆引き） |
| `doc/templates/coding-standards.md` | コーディング基準書（P規約・採点基準・Lint対応表） |
| `doc/templates/lint/README.md` | Lintヒント基盤（LintエラーをLLMの自己修正ヒントにする） |

## 仕様書（現在形の正典）

> DD = 変更の記録（フロー）、spec = 現在の姿（ストック）。
> DDアーカイブ時に同期チェックを行う（手順: spec-sync-check.md、Level 2 で doc/ 直下に配置）。

| パス | 説明 |
|------|------|
| `doc/spec/` | 画面・機能仕様書（テンプレート: `doc/templates/screen-spec-template.md`） |

## DD 設計文書

| パス | 説明 |
|------|------|
| `doc/DD/` | アクティブな設計文書 |
| `doc/DD/DD-INDEX.md` | DD一覧インデックス（`scripts/dd-index-gen.sh` で自動生成） |
| `doc/archived/DD/` | 完了済みDD（アーカイブ） |

## テンプレート

| ファイル | 説明 |
|---------|------|
| `doc/templates/dd_template.md` | DD標準テンプレート（受け入れ基準・リスク判定・完了前チェック） |
| `doc/templates/dd_template_bugfix.md` | バグ修正DD用 差分テンプレート |
| `doc/templates/dd_template_mock.md` | モック先行DD用 差分テンプレート |
| `doc/templates/dd_template_e2e.md` | E2E駆動DD用 差分テンプレート |
| `doc/templates/dd_template_tdd.md` | TDD駆動DD用 差分テンプレート |
| `doc/templates/guides.md` | DD作成ガイド（アプローチ選択・命名・サイズ管理・エビデンス） |
| `doc/templates/screen-spec-template.md` | 画面仕様書テンプレート（フロントマター status_check 付き） |

<!--
プロジェクトの成長に合わせてセクションを追加していく（参考: 成熟プロジェクトの構成例）:
- ## アプリケーション理解 (guide/)    — 全体ガイド・画面カタログ・用語集
- ## システム設計・仕様 (spec/)        — 機能仕様・状態遷移・権限モデル
- ## デプロイ (deploy/)               — デプロイ手順・ロールバック
- ## 運用 (operation/)                — サーバー構成・cron・監視
- ## セキュリティ (security/)          — 監査レポート・インシデント対応
-->
