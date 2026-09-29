#!/usr/bin/env bash
# =============================================================================
# doc-check.sh — DOC-MAP.md とドキュメント実体の整合性チェック
#
# チェック1（孤児検出）: doc/ 配下の .md のうち、DOC-MAP.md から
#   直接（ファイルパス）にも間接（親フォルダのパス）にも辿れないものを ERROR。
#   ※ doc/DD/ と doc/archived/ 配下は DD-INDEX.md が索引するため対象外。
# チェック2（リンク切れ検出）: DOC-MAP.md 内の `doc/...` パス参照のうち、
#   実体が存在しないものを ERROR（「未作成」と明記された行はスキップ）。
#
# 使い方（どのディレクトリから実行してもよい・CWD非依存）:
#   bash scripts/doc-check.sh
#   bash scripts/doc-check.sh --doc-dir doc
#
# パス設定はプロジェクトルート直下の .dd-config（DOC_DIR）で行う。
# 本スクリプトは全プロジェクト共通の配布物 — 直接編集しないこと。
#
# 終了コード: 0=整合 / 1=ERRORあり（precheck・CI に組み込める）
# 注意: Windows + Git Bash ではファイル数百件規模で fork コストにより遅くなる
#       可能性がある（scripts/README.md の dd-index-gen.sh の項を参照）。
# =============================================================================
set -euo pipefail

# --- プロジェクトルート解決と .dd-config 読み込み（CWD非依存） ---
# スクリプト自身の位置からルートを求める（想定配置: {ルート}/scripts/）。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
if [ ! -f "$PROJECT_ROOT/.dd-config" ]; then
    # scripts/ がルート直下にない配置向けフォールバック: 上方に .dd-config を探索
    _p="$SCRIPT_DIR"
    while [ -n "$_p" ] && [ "$_p" != "/" ] && [ "$_p" != "." ]; do
        if [ -f "$_p/.dd-config" ]; then PROJECT_ROOT="$_p"; break; fi
        _p="$(dirname "$_p")"
    done
fi
cd "$PROJECT_ROOT"

# .dd-config を安全に読む（source しない = 設定ファイル内の任意コード実行を防止）。
# KEY="value" / KEY=value 形式の行から、パスとして妥当な文字だけを1件抽出する。
dd_config_get() {  # dd_config_get KEY FILE
    [ -f "$2" ] || return 0
    sed -n "s|^[[:space:]]*${1}[[:space:]]*=[[:space:]]*\"\{0,1\}\([A-Za-z0-9 ._:/~-]*\)\"\{0,1\}[[:space:]]*\$|\1|p" "$2" 2>/dev/null | head -1
}
DOC_DIR="$(dd_config_get DOC_DIR .dd-config)"

DOC_DIR="${DOC_DIR:-doc}"
DOC_DIR="${DOC_DIR%/}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --doc-dir) DOC_DIR="$2"; shift 2 ;;
        *)         echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

MAP_FILE="$DOC_DIR/DOC-MAP.md"
if [ ! -f "$MAP_FILE" ]; then
    echo "ERROR: DOC-MAP が見つからない: $MAP_FILE" >&2
    exit 1
fi

errors=0

# =============================================================================
# チェック1: 孤児ドキュメント検出
# =============================================================================
while IFS= read -r f; do
    rel="${f#./}"
    case "$rel" in
        "$MAP_FILE")                              continue ;;  # DOC-MAP 自身
        "$DOC_DIR"/DD/*|"$DOC_DIR"/archived/*)    continue ;;  # DD-INDEX が索引
    esac

    # 直接参照（ファイルパスが DOC-MAP に登場するか）
    if grep -qF "$rel" "$MAP_FILE"; then
        continue
    fi

    # 間接参照（親フォルダがフォルダ単位で参照されているか。例: `doc/spec/`）
    reachable=0
    dir=$(dirname "$rel")
    while [ "$dir" != "$DOC_DIR" ] && [ "$dir" != "." ] && [ "$dir" != "/" ]; do
        if grep -qF "$dir/" "$MAP_FILE"; then
            reachable=1
            break
        fi
        dir=$(dirname "$dir")
    done

    if [ "$reachable" -eq 0 ]; then
        echo "ERROR [孤児]: $rel が DOC-MAP.md から辿れない（行を追加するか、フォルダ単位の参照に含める）"
        errors=$((errors + 1))
    fi
done < <(find "$DOC_DIR" -name '*.md' -type f | sort)

# =============================================================================
# チェック2: DOC-MAP 内のリンク切れ検出
# =============================================================================
while IFS= read -r line; do
    case "$line" in
        *未作成*) continue ;;  # 「未作成」と明記された行は将来予定としてスキップ
    esac
    while IFS= read -r ref; do
        [ -z "$ref" ] && continue
        case "$ref" in
            *'{'*|*'}'*) continue ;;  # プレースホルダ
        esac
        if [ ! -e "$ref" ]; then
            echo "ERROR [リンク切れ]: DOC-MAP.md が参照する $ref が存在しない"
            errors=$((errors + 1))
        fi
    done < <(printf '%s\n' "$line" | grep -oE "${DOC_DIR}/[A-Za-z0-9_./-]+" || true)
done < "$MAP_FILE"

# =============================================================================
# 結果
# =============================================================================
if [ "$errors" -gt 0 ]; then
    echo ""
    echo "✗ doc-check: ERROR ${errors}件。DOC-MAP.md と実体を同期すること。"
    exit 1
fi
echo "✓ doc-check: DOC-MAP.md と $DOC_DIR/ 配下の整合性 OK"
