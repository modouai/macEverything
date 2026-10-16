#!/usr/bin/env bash
# PoC-4 实机测量脚本 —— 隐藏态 CPU 采样 + 30 轮唤起延迟分布 + 外部「窗口在屏」对齐。
#
# 结论与口径见 docs/specs/v2-ui/poc-results.md「Task 4」节。
#
# 用法：
#   bash measure.sh build                     # 编译（release；拉依赖走 rsproxy 镜像）
#   bash measure.sh cpu-hide [SECS]           # 常隐 CPU 采样（默认 60s，每 ~5s 一点，共 12 点）
#   bash measure.sh show [SECS]               # 常显对照 CPU（默认 15s）
#   bash measure.sh warm [SECS]               # keep-warm 隐藏态 CPU（默认 60s）
#   bash measure.sh cycle                     # 30 轮 egui 路径（Visible(true)）唤起延迟分布
#   bash measure.sh native                    # 30 轮 native 路径（orderFrontRegardless）唤起延迟分布
#   bash measure.sh visible <cycle|native>    # 外部轮询：t0 →「窗口真正在屏」的对外延迟
#   bash measure.sh all                       # 全跑一遍并输出汇总
#
# 说明：
# - 延迟打点（SHOW_LATENCY_MS）是进程内口径（t0 → 首帧 ui() 进入），不含热键投递与上屏；
#   `visible` 用例用 CGWindowList 补「窗口真正在屏」的对外口径。
# - cpu-hide 期间窗口全程隐藏（启动首帧会亮一下，eframe 首帧后强制展出，同帧末被隐藏命令压回）。

set -u

BIN=./target/release/poc4-idle
T0_FILE=/tmp/poc4-t0.txt
POLLER=/tmp/poc4-visible.swift

build() {
  cargo build --release --config 'source.crates-io.replace-with="rsproxy"' \
              --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
}

# 统计 SHOW_LATENCY_MS 分布（中位 / P99 / 最大）。
stats() { # $1 = 日志
  grep -o 'SHOW_LATENCY_MS=[0-9.]*' "$1" | cut -d= -f2 | python3 -c '
import sys, statistics
v = sorted(float(x) for x in sys.stdin if x.strip())
if not v:
    print("(无打点)"); raise SystemExit
n = len(v)
p99 = v[min(n - 1, round(0.99 * (n - 1)))]
print(f"n={n} min={v[0]:.2f} median={statistics.median(v):.2f} mean={statistics.mean(v):.2f} p99={p99:.2f} max={v[-1]:.2f}")
'
}

# 区间 CPU 采样（top -l 2 的第二样本 = 区间值；第一样本是进程生命周期均值，弃用）。
sample_cpu() { # $1 = pid, $2 = 点数, $3 = 间隔秒
  local pid="$1" n="$2" gap="$3" i v
  for i in $(seq 1 "$n"); do
    v=$(top -l 2 -s 1 -pid "$pid" -stats cpu 2>/dev/null | tail -1 | tr -d ' ')
    echo "  sample[$i] cpu=${v:-?}%"
    sleep "$gap"
  done
}

cputime() { ps -o time= -p "$1" 2>/dev/null | tr -d ' '; }

# 常隐 / 常显 / keep-warm：CPU 采样 + 累计 CPU 时间差（交叉校验）。
case_cpu() { # $1 = 模式, $2 = 秒数, $3 = 标签
  local mode="$1" secs="$2" tag="$3"
  local log="/tmp/poc4-cpu-$tag.log"
  rm -f "$log"
  # 采样窗口 ≈ 6s 稳定 + 12×(1s 采样 + 4s 间隔) ≈ 66s，故进程存活时长取 secs+60。
  POC4_MODE="$mode" POC4_SECS="$((secs + 60))" POC4_POS=200,200 "$BIN" > "$log" 2>&1 &
  local pid=$!
  sleep 6   # 启动 + 首帧 + 隐藏命令生效（hide/warm）
  local t_before; t_before=$(cputime "$pid")
  echo "=== CPU 采样（mode=${mode}，每 ~5s 一点 × 12，起止累计 CPU: ${t_before} → ?）==="
  sample_cpu "$pid" 12 4
  local t_after; t_after=$(cputime "$pid")
  echo "--- 累计 CPU 时间：$t_before → ${t_after}（样本窗口 ~60s）---"
  wait "$pid" 2>/dev/null || true
  echo "--- 帧统计（${log}）---"
  grep -E '^PASS |WARM_TICK|SUMMARY' "$log" | tail -8
  echo "--- PASS 行总数：$(grep -c '^PASS ' "$log" || true)（隐藏稳态应只有启动帧）---"
}

case_cycle() { # $1 = 模式（cycle|native）
  local mode="${1:-cycle}"
  local log="/tmp/poc4-$mode.log"
  rm -f "$log" "$T0_FILE"
  echo "=== ${mode}：30 轮 [隐藏 2.5s → 显示 0.5s]（约 92s）==="
  POC4_MODE="$mode" POC4_POS=200,200 "$BIN" > "$log" 2>&1
  echo "--- 唤起延迟分布（进程内：t0 → 首帧 ui）---"
  stats "$log"
  echo "--- 通道成本（t0 → 首个 logic）---"
  grep -o 'SHOW_LOGIC_MS=[0-9.]*' "$log" | cut -d= -f2 | python3 -c '
import sys, statistics
v = sorted(float(x) for x in sys.stdin if x.strip())
n = len(v)
print(f"n={n} min={v[0]:.2f} median={statistics.median(v):.2f} max={v[-1]:.2f}")
'
  echo "--- ui 帧是否在窗口可见前跑到（win_visible_at_first_ui）---"
  grep -o 'win_visible_at_first_ui=[01]' "$log" | sort | uniq -c
  echo "--- 隐藏稳态是否有非驱动帧（隐藏期 PASS 分布）---"
  python3 - "$log" <<'PY'
import re, sys
lines = open(sys.argv[1]).read().splitlines()
t_seq = []
for l in lines:
    m = re.match(r'PASS seq=(\d+) kind=(\w+) win_visible=(\d) occluded=(\S+) t=([\d.]+)', l)
    if m:
        t_seq.append((m.group(2), m.group(3), float(m.group(5))))
# 窗口「不可见」（winit 口径）时跑到的帧：应只出现在 hide 命令帧与 macOS 遮挡事件帧，
# 隐藏稳态（两轮之间）不应有任何帧。
idle = [x for x in t_seq if x[1] == '0']
print(f"win_visible=0 的帧数 = {len(idle)}（= hide 命令帧 + 隐藏后 macOS 遮挡事件帧 + 各轮 show 的节流帧）")
# 隐藏稳态间隙：相邻隐藏帧间隔 > 1.5s 的说明期间完全无帧（= 事件循环在 Wait 上睡着）
gaps = [round(b[2] - a[2], 2) for a, b in zip(idle, idle[1:]) if b[2] - a[2] > 1.5]
print(f"隐藏期无帧间隔（>1.5s 的空窗，实为稳态）：{gaps[:6]}{' ...' if len(gaps) > 6 else ''}（共 {len(gaps)} 段）")
PY
}

# 外部口径：t0 →「窗口真正在屏」（CGWindowList 轮询，1ms 间隔）。
case_visible() { # $1 = 模式（cycle|native）
  local mode="${1:-native}"
  local plog="/tmp/poc4-visible-$mode.log"
  rm -f "$plog" "$T0_FILE"
  cat > "$POLLER" <<'SWIFT'
import CoreGraphics
import Foundation

// 用法：swift poc4-visible.swift <t0文件> <轮数>
// 读 t0 文件（"<纪元纳秒> <轮次>"），轮询 CGWindowList 等 poc4-idle 窗口在屏，
// 打印 t0 → 首次在屏的对外延迟。
let t0Path = CommandLine.arguments[1]
let rounds = Int(CommandLine.arguments[2]) ?? 10
let owner = "poc4-idle"

func onscreen() -> Bool {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else { return false }
    return list.contains { ($0[kCGWindowOwnerName as String] as? String) == owner }
}

var lastRound = 0
var lastT0 = 0.0
var wasOnscreen = false
var seen = 0
while seen < rounds {
    let on = onscreen()
    let now = Date().timeIntervalSince1970 * 1000.0
    if let s = try? String(contentsOfFile: t0Path, encoding: .utf8),
       let r = Int(s.split(separator: " ").last ?? "0"),
       let ns = Double(s.split(separator: " ").first ?? "0"), r > lastRound {
        lastRound = r
        lastT0 = ns / 1_000_000.0
        wasOnscreen = on
        if on {   // t0 写入与本采样点之间已在屏（<1 采样间隔）
            print(String(format: "VISIBLE_DELTA_MS=%.2f round=%d(<=采样间隔)", now - lastT0, r))
            seen += 1
        }
    } else if on && !wasOnscreen {
        print(String(format: "VISIBLE_DELTA_MS=%.2f round=%d", now - lastT0, lastRound))
        seen += 1
        wasOnscreen = true
    } else {
        wasOnscreen = on
    }
    usleep(1000)
}
SWIFT
  echo "=== visible[$mode]：外部轮询「窗口在屏」（CGWindowList，1ms 间隔）==="
  POC4_MODE="$mode" POC4_POS=200,200 POC4_T0_FILE="$T0_FILE" "$BIN" > "/tmp/poc4-$mode.log" 2>&1 &
  local pid=$!
  sleep 2
  swift "$POLLER" "$T0_FILE" 12 | tee "$plog"
  echo "--- 对外延迟分布 ---"
  grep -o 'VISIBLE_DELTA_MS=[0-9.]*' "$plog" | cut -d= -f2 | python3 -c '
import sys, statistics
v = sorted(float(x) for x in sys.stdin if x.strip())
if v:
    print(f"n={len(v)} min={v[0]:.2f} median={statistics.median(v):.2f} max={v[-1]:.2f}")
'
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

main() {
  local case_name="${1:-}"; shift || true
  case "$case_name" in
    build) build ;;
    cpu-hide) case_cpu hide "${1:-60}" hide ;;
    show) case_cpu show "${1:-15}" show ;;
    warm) case_cpu warm "${1:-60}" warm ;;
    cycle) case_cycle cycle ;;
    native) case_cycle native ;;
    visible) case_visible "${1:-native}" ;;
    all)
      build
      case_cpu hide 60 hide
      case_cpu show 15 show
      case_cpu warm 60 warm
      case_cycle cycle
      case_cycle native
      case_visible cycle
      case_visible native
      ;;
    *) sed -n '2,22p' "$0"; exit 1 ;;
  esac
}

main "$@"
