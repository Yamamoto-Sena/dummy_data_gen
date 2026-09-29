#!/usr/bin/env bash
# =============================================================================
# dd-index-gen.sh — DD-INDEX.md の全量再生成（高速版）
#
# DDフォルダとアーカイブの全DDファイルからメタデータを抽出し、
# DD-INDEX.md を生成する。冪等（何度実行しても同じ結果）。
#
# 高速化: ファイルごとのサブプロセス起動を排除し、単一 awk で一括処理。
# 200ファイルでも1秒以内で完了する。
#
# 使い方:
#   bash scripts/dd-index-gen.sh          # どのディレクトリから実行してもよい（CWD非依存）
#   bash scripts/dd-index-gen.sh --dd-dir doc/DD --archive-dir doc/archived/DD
#
# パス設定はプロジェクトルート直下の .dd-config（DD_DIR / ARCHIVE_DIR）で行う。
# 本スクリプトは全プロジェクト共通の配布物 — プロジェクト固有の値をここに直接
# 書かないこと（dd-know-how からの上書き更新で消えるため）。
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
DD_DIR="$(dd_config_get DD_DIR .dd-config)"
ARCHIVE_DIR="$(dd_config_get ARCHIVE_DIR .dd-config)"

# --- Default paths（.dd-config が無い場合の既定値） ---
DD_DIR="${DD_DIR:-doc/DD}"
ARCHIVE_DIR="${ARCHIVE_DIR:-doc/archived/DD}"
DD_DIR="${DD_DIR%/}"; ARCHIVE_DIR="${ARCHIVE_DIR%/}"
# .dd-config なしで既定が外れている場合の救済: docs/ 配置を自動検出
if [ ! -f .dd-config ] && [ ! -d "$DD_DIR" ] && [ -d "docs/DD" ]; then
    DD_DIR="docs/DD"
    [ -d "docs/archived/DD" ] && ARCHIVE_DIR="docs/archived/DD"
fi

# --- Parse arguments ---
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dd-dir)      DD_DIR="$2"; shift 2 ;;
        --archive-dir) ARCHIVE_DIR="$2"; shift 2 ;;
        *)             echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

INDEX_FILE="$DD_DIR/DD-INDEX.md"

# --- Validate directories ---
if [ ! -d "$DD_DIR" ]; then
    {
        echo "ERROR: DDフォルダが見つかりません: $DD_DIR（基準: $PROJECT_ROOT）"
        echo "  対処: プロジェクトルート直下に .dd-config を作成し、実パスを設定してください。例:"
        echo '    DD_DIR="doc/DD"'
        echo '    ARCHIVE_DIR="doc/archived/DD"'
        echo "  （一時的な指定は --dd-dir / --archive-dir 引数でも可）"
    } >&2
    exit 1
fi

# --- Collect DD files ---
# 識別子付きDD（DDK-001 / DDI-001 等。複数人・並行セッション運用）を拾うため
# glob は `DD-*` ではなく `DD*-*` を使い、dd-health.sh と同じ `^DD[A-Z]*-[0-9]`
# 判定で絞る（DD-INDEX.md や番号を持たないファイルはこの判定で除外される）。
# ソートキー（下方の `^DD[A-Z]*-` 除去）はもともと接頭辞対応済みで、収集だけが未対応だった。
DD_FILES=()
for f in "$DD_DIR"/DD*-*.md; do
    [ -f "$f" ] || continue
    [[ "${f##*/}" =~ ^DD[A-Z]*-[0-9] ]] || continue   # basename は fork するため ${f##*/} を使う
    DD_FILES+=("$f")
done

ARCHIVE_FILES=()
if [ -d "$ARCHIVE_DIR" ]; then
    for f in "$ARCHIVE_DIR"/DD*-*.md; do
        [ -f "$f" ] || continue
        [[ "${f##*/}" =~ ^DD[A-Z]*-[0-9] ]] || continue
        ARCHIVE_FILES+=("$f")
    done
fi

TOTAL=$(( ${#DD_FILES[@]} + ${#ARCHIVE_FILES[@]} ))

if [ "$TOTAL" -eq 0 ]; then
    # No DD files — write empty index
    cat > "$INDEX_FILE" <<'EMPTY_INDEX'
# DD 索引

> `bash scripts/dd-index-gen.sh` で自動生成。手動編集禁止。

## 進行中

| DD | 件名 | ステータス | 補足 |
|----|------|-----------|------|

## 保留・見送り

| DD | 件名 | ステータス | 理由 |
|----|------|-----------|------|

## 完了済み

| DD | 件名 | 主な成果 |
|----|------|---------|
EMPTY_INDEX
    echo "DD-INDEX.md updated: $INDEX_FILE (0 件)"
    exit 0
fi

# =============================================================================
# 高速メタデータ抽出: 単一 awk で全ファイルを一括処理
#
# 各ファイルの先頭6行だけ読み、以下を抽出:
#   - DD番号・タイトル: ファイル名から取得
#   - ステータス・補足: メタデータテーブル行（日付で始まるパイプ区切り行）の
#     4番目・5番目のフィールドから取得（3列の旧ヘッダ表では補足は空）
#
# 出力形式: section\tDD番号\tタイトル\tステータス\tソートキー\t補足
#   section: active（進行中） / hold（保留・見送り） / archived（完了済み）
#   補足は空になりうるため必ず最終フィールドに置く（bash の read はタブを IFS 空白
#   として扱い、連続タブ＝中間の空フィールドを潰して列ズレを起こすため）
# =============================================================================

ENTRIES_TMP=$(mktemp)
trap 'rm -f "$ENTRIES_TMP"' EXIT

# head -6 を全ファイルに一括実行し、awk で解析
# head の出力形式: "==> filepath <==" + 内容行
# NOTE: head は1回の呼び出しに複数ファイルを渡したときだけ "==> file <==" ヘッダーを
# 出す。DD側とアーカイブ側を別々に head すると、片側が1件のときヘッダーが出ず
# そのファイルが索引から漏れる（実測: アーカイブ1件が archived=0 になる）。
# 必ず1回の呼び出しにまとめる（合計1件のケースは後段のフォールバックが処理）。
head -6 "${DD_FILES[@]}" "${ARCHIVE_FILES[@]}" 2>/dev/null | awk -v ad="$ARCHIVE_DIR" '
# head -6 の出力を解析
# 単一ファイルの場合 "==> ... <==" ヘッダーが出ないため、事前に判定が必要
# → 呼び出し元で head に複数ファイルを渡すか、1ファイルでもヘッダーを出すようにする

BEGIN {
    FS = "|"
    filepath = ""
    status = "N/A"
    hosoku = ""
}

# ファイルヘッダー行
/^==> .* <==/ {
    # 前のファイルを出力
    if (filepath != "") {
        output_entry()
    }
    # 新しいファイルのパスを抽出
    gsub(/^==> /, "")
    gsub(/ <==.*/, "")
    filepath = $0
    status = "N/A"
    hosoku = ""
    next
}

# メタデータテーブル行（日付で始まるパイプ区切り）
/^\| *[0-9]{4}-[0-9]{2}-[0-9]{2}/ {
    if (status == "N/A") {
        # 4番目のフィールド = ステータス、5番目 = 補足（3列の旧ヘッダ表では空）
        s = $4
        gsub(/^ +| +$/, "", s)
        if (s != "") status = s
        h = $5
        gsub(/^ +| +$/, "", h)
        if (h != "") hosoku = h
    }
}

function output_entry() {
    # ファイル名からDD番号とタイトルを抽出
    fname = filepath
    # パスからファイル名部分だけ取得
    n = split(fname, parts, "/")
    basename = parts[n]
    # .md を除去
    sub(/\.md$/, "", basename)

    # DD番号とタイトルを分離
    idx = index(basename, "_")
    if (idx > 0) {
        dd_number = substr(basename, 1, idx - 1)
        title = substr(basename, idx + 1)
    } else {
        dd_number = basename
        title = "(タイトルなし)"
    }

    # ソートキー: DD-NNN → NNN の数値部分
    sort_key = dd_number
    gsub(/^DD[A-Z]*-/, "", sort_key)
    sub(/-.*/, "", sort_key)

    # セクション判定:
    #   archived: ARCHIVE_DIR 配下（前方一致）または archived?/ をパスに含む
    #             （固定正規表現 archived/ だけだと doc/DD/archive/ 等の配置で漏れる）
    #   hold:     ステータスが 保留/見送り で始まる（配置に関わらず優先）
    section = "active"
    if ((ad != "" && index(filepath, ad "/") == 1) || filepath ~ /archived?\//)
        section = "archived"
    if (index(status, "保留") == 1 || index(status, "見送り") == 1)
        section = "hold"

    printf "%s\t%s\t%s\t%s\t%s\t%s\n", section, dd_number, title, status, sort_key, hosoku
}

END {
    if (filepath != "") {
        output_entry()
    }
}
' > "$ENTRIES_TMP"

# --- 単一ファイルの場合のフォールバック ---
# head -6 が1ファイルだけだとヘッダーを出さないため、結果が空になる
if [ "$TOTAL" -eq 1 ] && [ ! -s "$ENTRIES_TMP" ]; then
    SINGLE_FILE=""
    if [ ${#DD_FILES[@]} -gt 0 ]; then
        SINGLE_FILE="${DD_FILES[0]}"
        LOCATION="active"
    else
        SINGLE_FILE="${ARCHIVE_FILES[0]}"
        LOCATION="archived"
    fi

    BASENAME=$(basename "$SINGLE_FILE" .md)
    if [[ "$BASENAME" == *_* ]]; then
        DD_NUM="${BASENAME%%_*}"
        TITLE="${BASENAME#*_}"
    else
        DD_NUM="$BASENAME"
        TITLE="(タイトルなし)"
    fi

    META_LINE=$(head -6 "$SINGLE_FILE" | grep -E '^\| *[0-9]{4}-[0-9]{2}-[0-9]{2}' | head -1 || true)
    STATUS=$(printf '%s' "$META_LINE" | awk -F'|' '{gsub(/^ +| +$/, "", $4); print $4}')
    HOSOKU=$(printf '%s' "$META_LINE" | awk -F'|' '{gsub(/^ +| +$/, "", $5); print $5}')
    [ -z "$STATUS" ] && STATUS="N/A"

    SECTION="$LOCATION"
    case "$STATUS" in
        保留*|見送り*) SECTION="hold" ;;
    esac

    SORT_KEY=$(echo "$DD_NUM" | sed 's/^DD[A-Z]*-//; s/-.*//')
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$SECTION" "$DD_NUM" "$TITLE" "$STATUS" "$SORT_KEY" "$HOSOKU" > "$ENTRIES_TMP"
fi

# --- Generate DD-INDEX.md ---
{
    echo "# DD 索引"
    echo ""
    echo "> \`bash scripts/dd-index-gen.sh\` で自動生成。手動編集禁止。"
    echo ""

    # 進行中（ソートキー降順）
    echo "## 進行中"
    echo ""
    echo "| DD | 件名 | ステータス | 補足 |"
    echo "|----|------|-----------|------|"
    # NOTE: grep が0件マッチで exit 1 を返すと pipefail+set -e で script 全体が落ちるため `|| true` で握りつぶす
    { grep '^active' "$ENTRIES_TMP" || true; } | sort -t$'\t' -k5,5nr | while IFS=$'\t' read -r _ dd_number title status _ hosoku; do
        printf '| %s | %s | %s | %s |\n' "$dd_number" "$title" "$status" "$hosoku"
    done
    echo ""

    # 保留・見送り（固定語彙 保留/見送り のステータスを自動検出。ソートキー降順）
    echo "## 保留・見送り"
    echo ""
    echo "| DD | 件名 | ステータス | 理由 |"
    echo "|----|------|-----------|------|"
    { grep '^hold' "$ENTRIES_TMP" || true; } | sort -t$'\t' -k5,5nr | while IFS=$'\t' read -r _ dd_number title status _ hosoku; do
        printf '| %s | %s | %s | %s |\n' "$dd_number" "$title" "$status" "$hosoku"
    done
    echo ""

    # 完了済み（ソートキー降順）
    echo "## 完了済み"
    echo ""
    echo "| DD | 件名 | 主な成果 |"
    echo "|----|------|---------|"
    # 「主な成果」列は補足欄（4列ヘッダ表の4列目）を優先し、無ければステータス欄を
    # 流用する（旧運用「完了時ステータスに成果要約を書く」の後方互換）。
    { grep '^archived' "$ENTRIES_TMP" || true; } | sort -t$'\t' -k5,5nr | while IFS=$'\t' read -r _ dd_number title status _ hosoku; do
        printf '| %s | %s | %s |\n' "$dd_number" "$title" "${hosoku:-$status}"
    done
} > "$INDEX_FILE"

# --- Report ---
# NOTE: grep -c は0件マッチ時に "0" を出力して exit 1 するため、`|| echo 0` だと "0\n0" が混入する。`|| true` で exit だけ握る。
active_count=$(grep -c '^active' "$ENTRIES_TMP" 2>/dev/null || true)
hold_count=$(grep -c '^hold' "$ENTRIES_TMP" 2>/dev/null || true)
archived_count=$(grep -c '^archived' "$ENTRIES_TMP" 2>/dev/null || true)

echo "DD-INDEX.md updated: $INDEX_FILE ($TOTAL 件: active=$active_count, hold=$hold_count, archived=$archived_count)"
