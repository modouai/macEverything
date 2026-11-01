#!/usr/bin/env bash
# 内存占用采样(memory-footprint 规格 S0 观测工具):循环记录 phys_footprint 与峰值。
# 用法:scripts/mem-watch.sh <pid|进程名> [采样间隔秒] [持续秒数]
#   pid 用进程名时按 pgrep 取首个匹配;同名多进程(daemon/GUI)先 pgrep -f 自行确认。
# 输出:CSV 到 stdout(timestamp,footprint_kb,lifetime_peak_kb);结束打印汇总。
# 口径:footprint -f bytes 的 Footprint(= phys_footprint,与活动监视器「内存」列一致)。
# 峰值:每 PEAK_EVERY 个样本用 vmmap 读一次进程生命周期峰值(重扫尖峰持续 10-30s,不会漏)。
set -euo pipefail

TARGET=${1:?用法: mem-watch.sh <pid|进程名> [间隔秒] [持续秒数]}
INTERVAL=${2:-2}
DURATION=${3:-0} # 0 = 采到手动中断
PEAK_EVERY=10

if [[ ! "$TARGET" =~ ^[0-9]+$ ]]; then
  TARGET=$(pgrep -f "$TARGET" | head -1)
  [[ -n "$TARGET" ]] || { echo "未找到进程: $1" >&2; exit 1; }
fi
kill -0 "$TARGET" 2>/dev/null || { echo "进程不存在: $TARGET" >&2; exit 1; }

echo "采样目标 pid=$TARGET 间隔=${INTERVAL}s 峰值插采=每${PEAK_EVERY}样本 (Ctrl-C 结束)" >&2
echo "timestamp,footprint_kb,lifetime_peak_kb"

footprint_kb() {
  /usr/bin/footprint -f bytes "$TARGET" 2>/dev/null |
    awk '/Footprint:/ { print int($5 / 1024) }'
}

peak_kb() {
  /usr/bin/vmmap "$TARGET" 2>/dev/null |
    awk '/Physical footprint \(peak\)/ { v=$4; sub(/[KMG]$/,"",v); if ($4 ~ /M$/) v*=1024; if ($4 ~ /G$/) v*=1048576; print int(v) }'
}

start=$(date +%s)
n=0 peak=0
while :; do
  if ! kill -0 "$TARGET" 2>/dev/null; then
    echo "进程 $TARGET 已退出,停止采样" >&2
    break
  fi
  cur=$(footprint_kb)
  if (( n % PEAK_EVERY == 0 )); then
    p=$(peak_kb); [[ -n "$p" ]] && peak=$p
  fi
  echo "$(date '+%H:%M:%S'),${cur:-0},${peak:-0}"
  n=$((n + 1))
  (( DURATION > 0 )) && (( $(date +%s) - start >= DURATION )) && break
  sleep "$INTERVAL"
done
echo "采样结束:共 $n 个样本(lifetime peak 取自 vmmap,见 CSV 第三列)" >&2
