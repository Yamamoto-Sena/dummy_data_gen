#!/usr/bin/env bash
# =============================================================================
# dd-health.sh — DD運用ヘルスチェック（静的分析）
#
# DDフォルダとアーカイブの全DDファイルを走査し、DD運用が「期待通り回っているか」
# を機械検証する。テレメトリや追加のログ記録は不要 — DD本体そのものを分析する。
#
# 全体レポート（どのディレクトリから実行してもよい・CWD非依存）:
#   bash scripts/dd-health.sh
#   bash scripts/dd-health.sh --dd-dir doc/DD --archive-dir doc/archived/DD
#
# パス設定はプロジェクトルート直下の .dd-config（DD_DIR / ARCHIVE_DIR）で行う。
# 本スクリプトは全プロジェクト共通の配布物 — プロジェクト固有の値をここに直接
# 書かないこと（dd-know-how からの上書き更新で消えるため）。
#
# 単一DDの即時チェック（作成直後・アーカイブ前のセルフチェック）:
#   bash scripts/dd-health.sh --dd DD-042          # アーカイブ前チェック（DA・ログも見る）
#   bash scripts/dd-health.sh --dd DD-042 --new    # 作成直後チェック（DA雛形は正常扱い）
#   bash scripts/dd-health.sh --dd DDK-001         # 識別子付きDD（複数人・並行セッション運用）も指定可
#
# 検出項目:
#   - クローズ漏れ（完了ステータスなのに未アーカイブ）
#   - 宙吊り（確認・レビュー待ちのまま WAIT_DAYS 超）
#   - 滞留（進行中のまま更新が STALE_DAYS 超停止）
#   - ログ形骸化（作成時スタブのまま）
#   - DA批判レビュー表の雛形残置
#   - テンプレ残骸（プレースホルダ・HTMLコメント）
#   - ステータス語彙lint（固定6種: 検討中/進行中/確認待ち/保留/見送り/完了 以外を検出）
#   - DD-INDEX.md の鮮度
#
# 終了コード: 常に 0。--strict 指定時のみ、要対応（⚠️）があれば 1。
# 高速化: dd-index-gen.sh と同様、ファイルごとのサブプロセス起動を排除し
#         単一 awk で一括処理する（Windows + Git Bash の fork コスト対策）。
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

TARGET_DD=""
NEW_MODE=0
STRICT=0
STALE_DAYS=30   # 進行中のまま更新停止と見なす日数
WAIT_DAYS=14    # 確認・レビュー待ちを宙吊りと見なす日数

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dd-dir)      DD_DIR="$2"; shift 2 ;;
        --archive-dir) ARCHIVE_DIR="$2"; shift 2 ;;
        --dd)          TARGET_DD="$2"; shift 2 ;;
        --new)         NEW_MODE=1; shift ;;
        --strict)      STRICT=1; shift ;;
        --stale-days)  STALE_DAYS="$2"; shift 2 ;;
        --wait-days)   WAIT_DAYS="$2"; shift 2 ;;
        *)             echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

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

TODAY=$(date +%Y-%m-%d)
# 日付差分用の単調スケール（y*372 + m*31 + d）。閾値判定用の近似で数日の誤差は許容。
TODAY_MONO=$(( 10#$(date +%Y) * 372 + 10#$(date +%m) * 31 + 10#$(date +%d) ))

# --- DDファイル収集（DD-INDEX.md や DD番号を持たないファイルは除外） ---
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

# =============================================================================
# メタデータ抽出: 単一 awk で全ファイルを一括処理
# 出力TSV: loc, DD番号, タイトル, 作成日, 更新日, ステータス, ログ日数, DA行数,
#          DA雛形行数, プレースホルダ数, HTMLコメント数, 受入基準セクション有無,
#          受入基準行数, 例示行数, Phase数, 🔬なしPhase一覧
# =============================================================================
extract() {
    local loc="$1"; shift
    [ $# -eq 0 ] && return 0
    awk -v loc="$loc" '
function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
function reset() {
    created=""; updated=""; status=""; section=""
    log_days=0; da_rows=0; da_scaf=0; ph=0; hc=0
    ac_seen=0; ac_rows=0; ac_example=0
    phases=0; cur_phase=""; cur_has=1; missing=""
}
function close_phase() {
    if (cur_phase != "" && cur_has == 0)
        missing = missing (missing == "" ? "" : "、") cur_phase
    cur_phase = ""; cur_has = 1
}
function emit(    n, parts, base, i, num, title) {
    if (fp == "") return
    close_phase()
    n = split(fp, parts, "/")
    base = parts[n]
    sub(/\.md$/, "", base)
    i = index(base, "_")
    if (i > 0) { num = substr(base, 1, i-1); title = substr(base, i+1) }
    else       { num = base; title = "" }
    printf "%s\t%s\t%s\t%s\t%s\t%s\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%s\n", \
        loc, num, title, created, updated, status, log_days, da_rows, da_scaf, \
        ph, hc, ac_seen, ac_rows, ac_example, phases, missing
}
FNR == 1 { emit(); reset(); fp = FILENAME }
{ sub(/\r$/, "") }
# ヘッダ表の値行（| 作成日 | 更新日 | ステータス |）
FNR <= 8 && created == "" && /^\|[ \t]*[0-9]{4}-[0-9]{2}-[0-9]{2}/ {
    split($0, f, "|")
    created = trim(f[2]); updated = trim(f[3]); status = trim(f[4])
}
/^## /  { close_phase(); section = trim(substr($0, 4)) }
/^### / {
    if (section ~ /ログ/) log_days++
    else if (section ~ /タスク一覧/ && $0 ~ /Phase/) {
        close_phase(); phases++
        # タイトルは最初のコロン（半角/全角）までで切る。sub(/[:：].*/) だと Windows の
        # gawk で .* が絵文字（非BMP文字）を跨げず切り残しが出るため、正規表現でなく
        # index/substr で切る（実測: 「Phase 1: 設計（📐 x）」→「Phase 1� x）」に化けた）
        cur_phase = trim(substr($0, 5))
        p = index(cur_phase, ":"); q = index(cur_phase, "：")
        if (q && (!p || q < p)) p = q
        if (p) cur_phase = substr(cur_phase, 1, p - 1)
        cur_phase = trim(cur_phase)
        cur_has = ($0 ~ /Phase[ \t]*0/) ? 1 : 0   # Phase 0 は🔬対象外
    }
}
# 🔬絵文字（非BMP）は Windows の gawk の正規表現で照合できない（出力への印字は正常・
# BMP文字の照合は正常、と実測）ため、タスク文言（機械検証/机上突合/机上検証）で判定する
section ~ /タスク一覧/ && /機械検証|机上突合|机上検証/ { cur_has = 1 }
/\{番号\}|\{タイトル\}|\{日付\}/ { ph++ }
/<!--/ { hc++ }
section ~ /DA/ && /^\|[ \t]*[0-9]+[ \t]*\|/ {
    da_rows++
    if ($0 ~ /具体的に記述|\|[ \t]*\.\.\.[ \t]*\||高\/中\/低|C\/W\/I/) da_scaf++
}
section ~ /受け入れ基準/ {
    ac_seen = 1
    if ($0 ~ /^\|[ \t]*[0-9]+[ \t]*\|/) {
        ac_rows++
        if ($0 ~ /例[:：]/) ac_example++
    }
}
END { emit() }
' "$@"
}

# =============================================================================
# 単一DDモード（--dd）: 作成直後・アーカイブ前の即時セルフチェック
# =============================================================================
if [ -n "$TARGET_DD" ]; then
    raw="$TARGET_DD"
    # 接頭辞（DD / dd）・識別子（DDK の K、DDSA の SA 等）・ハイフンを分解する。
    # 識別子は捨てない — DD-001 と DDK-001 が併存すると `DD*-001` が両方にマッチし、
    # 辞書順で誤った方を拾うため。
    # 例: DD-042 → ident="" num=042 / DDK-001 → ident=K num=001 / DD-001-1 → ident="" num=001-1
    ident=""
    if [[ "$raw" =~ ^[Dd][Dd]([A-Za-z]*)-?(.+)$ ]]; then
        ident="${BASH_REMATCH[1]}"; raw="${BASH_REMATCH[2]}"
    fi
    # ファイル名の識別子は大文字（収集時の ^DD[A-Z]*- 判定と同じ）。glob は大小を
    # 区別するため、小文字入力（ddk-001 等）はここで正規化する。
    [ -n "$ident" ] && ident=$(printf '%s' "$ident" | tr '[:lower:]' '[:upper:]')
    pad="$raw"
    [[ "$raw" =~ ^[0-9]+$ ]] && pad=$(printf "%03d" "$((10#$raw))")

    # 識別子が明示されていればそれに限定。無指定なら素のDDを優先し、無ければ識別子付きへ広げる。
    # パターンは文字列のまま保持する（ここで展開させない）。展開は下の `"$d"/$p` で行う。
    if [ -n "$ident" ]; then
        PATTERNS=("DD${ident}-${pad}_*.md" "DD${ident}-${pad}.md" "DD${ident}-${raw}_*.md" "DD${ident}-${raw}.md")
    else
        PATTERNS=("DD-${pad}_*.md" "DD-${pad}.md" "DD-${raw}_*.md" "DD-${raw}.md"
                  "DD*-${pad}_*.md" "DD*-${pad}.md" "DD*-${raw}_*.md" "DD*-${raw}.md")
    fi

    FILE=""
    IS_ACTIVE=1
    for d in "$DD_DIR" "$ARCHIVE_DIR"; do
        [ -d "$d" ] || continue
        for p in "${PATTERNS[@]}"; do
            for cand in "$d"/$p; do
                if [ -f "$cand" ]; then FILE="$cand"; break 3; fi
            done
        done
        IS_ACTIVE=0
    done
    if [ -z "$FILE" ]; then
        echo "ERROR: $TARGET_DD が見つかりません（$DD_DIR / $ARCHIVE_DIR）" >&2
        exit 1
    fi

    RC=0
    extract "single" "$FILE" | awk -F'\t' \
        -v today="$TODAY_MONO" -v fname="$(basename "$FILE")" -v is_active="$IS_ACTIVE" -v newmode="$NEW_MODE" '
function mono(d,    a) { if (split(d, a, "-") != 3) return -1; return a[1]*372 + a[2]*31 + a[3] }
function vocab_ok(s) { return (s == "検討中" || s == "進行中" || s == "確認待ち" || s == "保留" || s == "見送り" || s == "完了") }
{
    created=$4; upd=$5; st=$6; logd=$7+0; dar=$8+0; das=$9+0
    ph=$10+0; hc=$11+0; ac_seen=$12+0; ac_rows=$13+0; ac_ex=$14+0
    phases=$15+0; missing=$16
    warn = 0
    print "### " fname " ヘルスチェック"
    print ""
    if (created == "" || st == "") { print "- ⚠️ ヘッダ表（作成日/更新日/ステータス）を読み取れない → 冒頭の表を確認"; warn++ }
    else if (!vocab_ok(st)) { print "- ⚠️ ステータス「" st "」が固定語彙外 → 検討中/進行中/確認待ち/保留/見送り/完了 のいずれかにし、説明・成果は補足列（4列目）へ"; warn++ }
    else print "- ✅ ヘッダ表: 作成 " created " / 更新 " upd " / ステータス「" st "」"
    if (ph + hc > 0) { print "- ⚠️ テンプレ残骸: プレースホルダ " ph "箇所・HTMLコメント " hc "箇所 → 除去する"; warn++ }
    else print "- ✅ テンプレ残骸なし"
    if (ac_seen == 0) print "- ℹ️ 受け入れ基準セクションなし（旧テンプレ由来なら可）"
    else if (ac_rows == 0 || ac_rows == ac_ex) { print "- ⚠️ 受け入れ基準が未記入（例示行のみ）→ 操作→期待結果の形で記入する"; warn++ }
    else print "- ✅ 受け入れ基準 " ac_rows "件"
    if (missing != "") { print "- ⚠️ 🔬機械検証タスクがないPhase: " missing; warn++ }
    else if (phases > 0) print "- ✅ 全Phaseに🔬機械検証タスクあり（" phases " Phase）"
    if (logd <= 1) print "- ℹ️ ログは作成時の1件のみ（作業を進めたら日付ごとに追記）"
    else print "- ✅ ログ " logd "日分"
    if (dar > 0 && das >= dar) {
        if (newmode) print "- ℹ️ DA表は雛形（Phase完了時に記録する）"
        else { print "- ⚠️ DA批判レビュー表が雛形のまま → 実施して記録するか、不要な理由をログに残す"; warn++ }
    }
    else if (dar > 0) print "- ✅ DA記録 " dar "行"
    else print "- ℹ️ DA記録なし（v7テンプレでは記録表なしが標準。所見はログへ）"
    if (is_active == 1 && st ~ /完了|実装済|反映済|[Dd]one|見送り/ && st !~ /待ち/)
        print "- 🗄️ ステータスが終端（完了/見送り系）のまま未アーカイブ → /dd archive を検討"
    exit (warn > 0 ? 10 : 0)
}' || RC=$?

    if [ "$STRICT" -eq 1 ] && [ "$RC" -eq 10 ]; then exit 1; fi
    exit 0
fi

# =============================================================================
# 全体レポートモード
# =============================================================================
TSV=$(mktemp)
trap 'rm -f "$TSV"' EXIT

{
    if [ ${#DD_FILES[@]} -gt 0 ];      then extract "active"   "${DD_FILES[@]}"; fi
    if [ ${#ARCHIVE_FILES[@]} -gt 0 ]; then extract "archived" "${ARCHIVE_FILES[@]}"; fi
} > "$TSV"

echo "# DDヘルスレポート（$TODAY）"
echo ""
echo "対象: \`$DD_DIR\`（アクティブ ${#DD_FILES[@]}件） / \`$ARCHIVE_DIR\`（アーカイブ ${#ARCHIVE_FILES[@]}件）"
echo ""

RC=0
awk -F'\t' -v today="$TODAY_MONO" -v staled="$STALE_DAYS" -v waitd="$WAIT_DAYS" '
function mono(d,    a) { if (split(d, a, "-") != 3) return -1; return a[1]*372 + a[2]*31 + a[3] }
function app(a, b) { return (a == "" ? b : a " / " b) }
function vocab_ok(s) { return (s == "検討中" || s == "進行中" || s == "確認待ち" || s == "保留" || s == "見送り" || s == "完了") }
{
    loc=$1; num=$2; title=$3; upd=$5; st=$6
    logd=$7+0; dar=$8+0; das=$9+0; ph=$10+0; hc=$11+0
    grand++
    if (logd >= 2) multilog++
    if (dar == 0) da_none++
    else if (das >= dar) da_scaffold++
    else da_filled++
    if (loc != "active") next

    active++
    m = mono(upd)
    age = (m < 0) ? -1 : today - m
    w = ""
    if (st == "" || st == "N/A") w = app(w, "⚠️ ステータス未設定")
    else if (!vocab_ok(st)) w = app(w, "🏷️ 語彙外ステータス「" st "」→ 固定6種+補足列へ")
    if (st ~ /待ち|確認可/) {
        if (age > waitd) w = app(w, "⏸️ 確認・レビュー待ちのまま約" age "日")
    } else if (st ~ /完了|実装済|反映済|[Dd]one|アーカイブ|見送り/) {
        w = app(w, "🗄️ クローズ漏れ（「" st "」だが未アーカイブ）")
    } else if (age > staled) {
        w = app(w, "🧊 更新が約" age "日停止（「" st "」のまま）")
    }
    if (logd <= 1 && age > waitd) w = app(w, "📝 ログが作成時のまま")
    if (dar > 0 && das >= dar)    w = app(w, "😈 DA表が雛形のまま")
    if (ph + hc > 0)              w = app(w, "🧹 テンプレ残骸" (ph + hc) "箇所")
    if (w != "") {
        nw++
        warns[nw] = "- **" num "**" (title != "" ? "（" title "）" : "") ": " w
    }
}
END {
    printf "## サマリー\n\n"
    printf "| 指標 | 値 |\n|------|----|\n"
    printf "| アクティブDD | %d件（要対応 %d件） |\n", active + 0, nw + 0
    printf "| アーカイブ済みDD | %d件 |\n", grand - active
    if (grand > 0) {
        printf "| ログ活性率（2日以上の作業ログを持つDD） | %d%%（%d/%d件） |\n", multilog * 100 / grand, multilog, grand
        printf "| DA記入率（記録表を持つ旧テンプレDD向け指標） | %d%%（記入 %d / 雛形のまま %d / 記録なし %d） |\n", da_filled * 100 / grand, da_filled, da_scaffold, da_none
    }
    printf "\n## 要対応（アクティブDD）\n\n"
    if (nw == 0) print "なし ✅"
    else for (i = 1; i <= nw; i++) print warns[i]
    exit (nw > 0 ? 10 : 0)
}
' "$TSV" || RC=$?

echo ""
echo "## ステータス語彙（アクティブDD・固定6種チェック）"
echo ""
echo "| ステータス | 件数 | 判定 |"
echo "|-----------|------|------|"
awk -F'\t' '$1 == "active" { print ($6 == "" ? "(ヘッダ表なし)" : $6) }' "$TSV" | sort | uniq -c | sort -rn | head -15 | \
    awk '{
        n = $1
        s = $0; sub(/^[ \t]*[0-9]+ /, "", s)
        if (s == "(ヘッダ表なし)") mark = "—"
        else if (s == "検討中" || s == "進行中" || s == "確認待ち" || s == "保留" || s == "見送り" || s == "完了") mark = "✅"
        else mark = "⚠️ 語彙外"
        printf "| %s | %s | %s |\n", s, n, mark
    }'
echo ""
echo "> ステータスは固定6種（検討中/進行中/確認待ち/保留/見送り/完了）のみ。説明・成果は補足列（4列目）へ書く。語彙外の既存DDは、次にそのDDを触るタイミングで移行すればよい（アーカイブ済みへの遡及は不要）。"

# --- DD-INDEX.md の鮮度（アクティブDDより古ければ再生成を促す） ---
if [ -f "$DD_DIR/DD-INDEX.md" ] && [ ${#DD_FILES[@]} -gt 0 ]; then
    newest=$(ls -t -- "${DD_FILES[@]}" "$DD_DIR/DD-INDEX.md" | head -1)
    if [ "$(basename "$newest")" != "DD-INDEX.md" ]; then
        echo ""
        echo "⚠️ DD-INDEX.md より新しいDDファイルがあります（$(basename "$newest")）→ \`bash scripts/dd-index-gen.sh\` で再生成を推奨"
    fi
fi

if [ "$STRICT" -eq 1 ] && [ "$RC" -eq 10 ]; then exit 1; fi
exit 0
