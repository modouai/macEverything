#!/usr/bin/env bash
# 六项基准(对照 product.md 目标):首扫 / 查询 P99 / 拼音查询 P99 / 增量时效 / 落盘 / 停机重放。
# 用法:scripts/bench.sh [合成树根] [文件数]
# 隔离:MCE_ROOTS 指向合成树(不扫真实主目录),HOME 指向临时数据目录。
set -euo pipefail

ROOT=${1:-/tmp/mce-bench}
TOTAL=${2:-1000000}
BIN=${BIN:-target/release/macEverything}
BENCH_HOME=$(mktemp -d /tmp/mce-bench-home.XXXXXX)
export MCE_ROOTS="$ROOT"
# 合成树中文名占比:默认 20;0 = 纯 ASCII(旧基线);100 = 全中文名极端树
export MCE_GEN_CN_PCT="${MCE_GEN_CN_PCT:-20}"

# 小目录走废纸篓(trash 不在时装提示保留,不静默);合成树体积大,不走废纸篓。
trash_dir() {
  local d=$1
  [ -e "$d" ] || return 0
  if command -v trash >/dev/null 2>&1; then
    trash "$d" 2>/dev/null || echo "警告: trash 失败,留下 $d 待手动清理" >&2
  else
    echo "提示: 未找到 trash 命令,留下 $d 待手动清理" >&2
  fi
}

cleanup() {
  HOME="$BENCH_HOME" "$BIN" stop >/dev/null 2>&1 || true
  rm -f "${TIMES:-}"
  trash_dir "$BENCH_HOME"
  # 合成树默认保留(复查/复跑省去重建的分钟级开销);BENCH_PURGE_TREE=1 彻底删除
  if [ "${BENCH_PURGE_TREE:-0}" = "1" ]; then
    rm -rf "$ROOT"
    echo "已删除合成树 $ROOT"
  else
    echo "合成树保留于 $ROOT(BENCH_PURGE_TREE=1 重跑可删,或手动清理)"
  fi
}
trap cleanup EXIT

now() { python3 -c 'import time;print(f"{time.time():.3f}")'; }
elapsed() { python3 -c "print(f'{$2-$1:.2f}')"; }

# 采样 N 次查询,输出中位/P99/最大;<label> 仅用于打印,<mode> = random | pinyin
query_p99() { # $1=label $2=mode
  local label=$1 mode=$2 times kw i
  times=$(mktemp /tmp/mce-qp.XXXXXX)
  for i in $(seq 1 200); do
    if [ "$mode" = "pinyin" ]; then
      if [ $((i % 2)) -eq 0 ]; then kw="shuxue"; else kw="shux"; fi
    else
      kw="file_$(printf %07d $(( (RANDOM * 32768 + RANDOM) % TOTAL )))"
    fi
    HOME="$BENCH_HOME" "$BIN" query "$kw" 2>/dev/null \
      | head -1 | grep -oE '匹配 [0-9]+\.[0-9]+' | grep -oE '[0-9]+\.[0-9]+' >> "$times" || true
  done
  local n
  n=$(wc -l < "$times" | tr -d ' ')
  if [ "$n" -lt 150 ]; then
    echo "  $label 样本不足($n/200),基准失败" >&2
    rm -f "$times"
    return 1
  fi
  python3 - "$times" "$label" <<'PY'
import sys
xs = sorted(float(l) for l in open(sys.argv[1]) if l.strip())
n = len(xs)
print(f"  {sys.argv[2]}样本 {n} · 中位 {xs[n//2]:.1f} ms · P99 {xs[min(n-1, int(n*0.99))]:.1f} ms · 最大 {xs[-1]:.1f} ms")
PY
  rm -f "$times"
}

wait_ready() { # 轮询到状态=就绪,输出耗时
  local t0 t1
  t0=$(now)
  for _ in $(seq 1 1200); do
    if HOME="$BENCH_HOME" "$BIN" status 2>/dev/null | grep -q "状态: 就绪"; then
      t1=$(now); elapsed "$t0" "$t1"; return 0
    fi
    sleep 0.1
  done
  echo "TIMEOUT" >&2; return 1
}

echo "== 0. 生成合成树($TOTAL 文件)@ $ROOT =="
echo "  中文名占比: ${MCE_GEN_CN_PCT}%"
(cd scripts/spike-readdir && cargo run --release --quiet -- gen "$ROOT" "$TOTAL")

echo "== 1. 首扫(到就绪,红线 ≤ 5s) =="
HOME="$BENCH_HOME" "$BIN" start >/dev/null 2>&1 &
sleep 0.3
printf "  首扫: %ss\n" "$(wait_ready)"
HOME="$BENCH_HOME" "$BIN" status | grep -E "条目数|索引生成" | sed 's/^/  /'

echo "== 2. 查询 P99(200 次随机关键词,红线 P99 < 10ms) =="
# 只取「匹配」段:输出含「匹配 X ms · 读取信息 Y ms」两段,泛取会混入 stat 耗时
query_p99 "查询" random

echo "== 2b. 拼音查询 P99(200 次:shuxue/shux 交替,红线 P99 < 10ms) =="
# 预热:反查表 OnceLock 懒构建计入首查,不占 P99 样本;命中也兼作功能性断言
if [ "$MCE_GEN_CN_PCT" -gt 0 ]; then
  HITS=$(HOME="$BENCH_HOME" "$BIN" query "shuxue" 2>/dev/null | head -1 || true)
  if ! printf '%s\n' "$HITS" | grep -qE '共 [1-9][0-9]* 项'; then
    echo "拼音查询无命中,基准失败" >&2
    exit 1
  fi
else
  echo "  MCE_GEN_CN_PCT=0(纯 ASCII 树),跳过拼音命中断言"
  HOME="$BENCH_HOME" "$BIN" query "shuxue" >/dev/null 2>&1 || true
fi
query_p99 "拼音" pinyin

echo "== 3. 增量时效(建文件到可搜,红线 ≤ 500ms) =="
PROBE="$ROOT/__inc_probe__.txt"
: > "$PROBE"
T0=$(now)
for _ in $(seq 1 100); do
  if HOME="$BENCH_HOME" "$BIN" query "__inc_probe__" 2>/dev/null | grep -q "共 1 项"; then break; fi
  sleep 0.05
done
T1=$(now)
printf "  增量可搜: %ss(含查询往返,粒度 ~50ms)\n" "$(elapsed "$T0" "$T1")"
rm -f "$PROBE"

echo "== 4. 落盘(stop 响应即落盘保证) =="
T0=$(now)
HOME="$BENCH_HOME" "$BIN" stop >/dev/null
T1=$(now)
printf "  stop+落盘: %ss\n" "$(elapsed "$T0" "$T1")"
ls -lh "$BENCH_HOME/Library/Application Support/macEverything/index.bin" | awk '{print "  索引文件: "$5}'

echo "== 5. 停机重放(停机窗口改 10000 文件,红线 ≤ 2s / 全量重扫为兜底) =="
mkdir -p "$ROOT/__replay_window__"
seq 1 10000 | xargs -P 16 -I{} touch "$ROOT/__replay_window__/r{}.txt"
HOME="$BENCH_HOME" "$BIN" start >/dev/null 2>&1 &
sleep 0.3
printf "  冷启动到就绪(含重放): %ss\n" "$(wait_ready)"
HITS=$(HOME="$BENCH_HOME" "$BIN" query "r9999" 2>/dev/null | head -1)
printf "  重放补齐抽验: %s\n" "$HITS"
# 先停 daemon 再删窗口目录:运行中删除会与监听/事件处理竞态
HOME="$BENCH_HOME" "$BIN" stop >/dev/null
rm -rf "$ROOT/__replay_window__"
echo "== 基准完成 =="
