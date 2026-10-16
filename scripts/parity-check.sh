#!/usr/bin/env bash
# 对拍(验收「准」轴):索引条目集 vs find 递归枚举集,同排除/同符号链接策略,
# 差异必须为空。语义复刻 scanner:符号链接不入索引;排除目录整棵剪枝
# (绝对路径前缀式 + 任意层级相对名式,见 scanner::is_excluded)。
#
# 用法:scripts/parity-check.sh [扫描根...]   (缺省 = $HOME 与 /Applications)
# 环境:MCE_INDEX 指定索引文件;MCE_EXCLUDES 覆盖排除列表(缺省从 config.toml 读)
# 退出码:0 = 零差异;1 = 有差异或执行失败
set -euo pipefail

BIN=${BIN:-target/release/macEverything}
EXPORT=${EXPORT:-target/release/examples/export_paths}
DATA_DIR=${MCE_DATA_DIR:-$HOME/Library/Application Support/macEverything}
INDEX=${MCE_INDEX:-$DATA_DIR/index.bin}

ROOTS=("$@")
if [ ${#ROOTS[@]} -eq 0 ]; then ROOTS=("$HOME" "/Applications"); fi

if [ ! -f "$INDEX" ]; then
  echo "索引文件不存在: $INDEX(先 macEverything start)" >&2
  exit 1
fi

# 排除列表:优先环境变量,否则从 config.toml 提取引号内字符串(每行一条);
# 配置文件不存在时用与 engine::config::default_config 一致的默认三项
if [ -n "${MCE_EXCLUDES:-}" ]; then
  EXCLUDES="$MCE_EXCLUDES"
elif [ -f "$DATA_DIR/config.toml" ]; then
  EXCLUDES=$(sed -n 's/.*"\(.*\)".*/\1/p' "$DATA_DIR/config.toml" | grep -v '^excluded$' || true)
else
  EXCLUDES="~/Library
node_modules
.git"
fi
echo "== 对拍:${#ROOTS[@]} 个扫描根 =="
echo "  索引: $INDEX"
echo "  排除: $(echo "$EXCLUDES" | tr '\n' ' ')"

TMP=$(mktemp -d /tmp/mce-parity.XXXXXX)
# 失败时保留现场(差异清单可复查);成功即清理
trap 'if [ "$RC" = "0" ]; then rm -rf "$TMP"; else echo "现场保留于 $TMP(差异清单可复查)" >&2; fi' EXIT
RC=1

echo "== 1. 导出索引条目 =="
"$EXPORT" "$INDEX" > "$TMP/index.txt"
LC_ALL=C sort -o "$TMP/index.txt" "$TMP/index.txt"
echo "  索引条目: $(wc -l < "$TMP/index.txt" | tr -d ' ')"

echo "== 2. find 递归枚举(同排除/同符号链接策略) =="
# 排除谓词:目录名命中相对名(任意层级)或路径命中绝对前缀 → 整棵剪枝
# 条件间必须显式 -o 分隔(数组元素是独立 argv,不加则相邻条件是 AND 关系,
# 「-name node_modules」会被并到前一个 -path 上永不独立命中——曾致全量对拍
# 误报 194 万条 node_modules 差异)
PRUNE_ARGS=()
append_cond() {
  if [ ${#PRUNE_ARGS[@]} -gt 0 ]; then PRUNE_ARGS+=( -o ); fi
  PRUNE_ARGS+=( "$@" )
}
while IFS= read -r ex; do
  [ -z "$ex" ] && continue
  # ~ 展开(与 reconcile::expand_excludes 同规则)
  case "$ex" in
    "~") ex="$HOME" ;;
    "~/"*) ex="$HOME/${ex#\~/}" ;;
  esac
  case "$ex" in
    /*) append_cond -path "$ex" -o -path "$ex/*" ;;
    *) append_cond -type d -name "$ex" ;;
  esac
done <<< "$EXCLUDES"
# 排除列表为空时给出恒假条件,避免 \( \) 空组语法错误
if [ ${#PRUNE_ARGS[@]} -eq 0 ]; then PRUNE_ARGS=( -false ); fi

find "${ROOTS[@]}" \
  \( "${PRUNE_ARGS[@]}" \) -prune -o \
  ! -type l -print > "$TMP/find.txt" 2>"$TMP/find.err" || true
if [ -s "$TMP/find.err" ]; then
  echo "  (find 有权限告警 $(wc -l < "$TMP/find.err" | tr -d ' ') 条,与索引侧权限缺口对应)"
fi
# 索引含根条目自身(名字 = 完整路径),find 的根不自列 → 补齐对齐
printf '%s\n' "${ROOTS[@]}" >> "$TMP/find.txt"
LC_ALL=C sort -u -o "$TMP/find.txt" "$TMP/find.txt"
echo "  find 条目: $(wc -l < "$TMP/find.txt" | tr -d ' ')"

echo "== 3. 比对 =="
if diff -q "$TMP/index.txt" "$TMP/find.txt" >/dev/null; then
  echo "  ✅ 零差异:索引条目集与文件系统一致($(wc -l < "$TMP/index.txt" | tr -d ' ') 条)"
  RC=0
  exit 0
fi
# 差异清单先落文件再取样展示(直接 comm|head 会因 SIGPIPE + pipefail 提前退出)
comm -23 "$TMP/index.txt" "$TMP/find.txt" > "$TMP/only_index.txt" || true
comm -13 "$TMP/index.txt" "$TMP/find.txt" > "$TMP/only_find.txt" || true
echo "  ❌ 有差异:" >&2
echo "  索引独有 $(wc -l < "$TMP/only_index.txt" | tr -d ' ') 条(前 20):" >&2
head -20 "$TMP/only_index.txt" | sed 's/^/    /' >&2 || true
echo "  find 独有 $(wc -l < "$TMP/only_find.txt" | tr -d ' ') 条(前 20):" >&2
head -20 "$TMP/only_find.txt" | sed 's/^/    /' >&2 || true
exit 1
