# AGENTS.md

> 全エージェント共通の正本（Codex は直接、Claude Code は CLAUDE.md の `@AGENTS.md` インポート経由で読む）

## プロジェクト概要

YAML設定ファイル（`schema.yaml`）で定義した列に沿って日本語ダミーデータをCSV/SQL/JSON/Excel(xlsx)に書き出すRust製CLIツール。生成エンジン本体（`src/lib.rs`）は `dummygen_jp_gui` からライブラリとして呼ばれる。エンジンの詳細な仕様・設計判断は [CLAUDE.md](CLAUDE.md) に集約されている（本ファイルはDD運用の設定のみ）。

- **技術スタック**: Rust (edition 2024) / clap / serde+serde_yaml / rayon / rust_xlsxwriter
- **ドキュメント一覧**: `doc/DOC-MAP.md`（全ドキュメントの場所と目的。迷ったらまずここ）

## コマンド

| コマンド | 用途 |
|---------|------|
| `cargo build` | ビルド |
| `cargo run -- --config schema.yaml --format csv --seed 42` | 実行例 |
| `cargo test` | テスト実行 |
| `cargo check` | 型チェック（`dummygen_jp_gui/src-tauri`・`src-server` の依存側も合わせて確認） |
| `bash scripts/doc-check.sh` | ドキュメント整合性チェック（DOC-MAP孤児・リンク切れ） |

## DD設定

- **DDフォルダ**: `doc/DD/` / **アーカイブ**: `doc/archived/DD/` / **テンプレート**: `doc/templates/dd_template.md`
- **パス設定**: ルート直下の `.dd-config`（スクリプト・フックはここを読む。上の実パスと常に一致させる）
- **ステータス**: 固定6種（検討中/進行中/確認待ち/保留/見送り/完了）+ 補足列。語彙ルール: `doc/templates/guides.md` §3
- **スキル**: `/dd new|list|log|archive|search|rebuild-index|health`（Claude Code: `.claude/skills/` / Codex: `.agents/skills/` — 同一内容のミラー）
- **開発フロー**: DD作成 → 仕様確認 → 実装 → 検証 → 完了（いきなりコードを書かない）
- **レビュー**: DDタスクに組み込まない — 完了報告を見た人間が別モデルへ都度指示（`doc/templates/guides.md` §10）
- **コミット**: `DD-{番号}: 概要` 形式。stage は対象ファイルを明示する — `git add -A` は並行セッションの作業や秘匿ファイルを巻き込むため使わない

## コーディング規約

- 基準書: `doc/templates/coding-standards.md`（コードレビューはこの基準で評価する）
- エンジンの列タイプ・アーキテクチャの詳細ルールは [CLAUDE.md](CLAUDE.md) が正本（本ファイルとは役割が別）

## ドキュメント更新義務

- `doc/` にドキュメントを追加・移動したら `doc/DOC-MAP.md` も更新する
- 既存の `docs/`（requirements.md・spec.md）は本DD導入以前からのドキュメントで、`doc/`（DD運用用）とは別物。統合はせず併存させる

## エージェント別の注意

- 編集ガード等の hooks は Claude Code 固有。Codex 等ではガードが効かないがルールは同じ: DD-INDEX.md は直接編集せず `bash scripts/dd-index-gen.sh` で再生成する
