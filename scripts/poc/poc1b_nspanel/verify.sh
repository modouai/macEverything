#!/usr/bin/env bash
# PoC-1 续（poc1b）实机验证脚本 —— 逐项取证：CGWindowList 清单 + 截图。
#
# 用法：
#   bash verify.sh smoke            # 桌面单跑：换类 + 渲染 + 清单取证
#   bash verify.sh input-diff       # 输入链路差分：mode=subclass vs mode=none（合成按键自测）
#   bash verify.sh space <mode> [k=v ...]
#       跨全屏 Space 主测：mode ∈ {subclass, plain, none}，其余环境变量追加在末尾
#       例：bash verify.sh space subclass
#           bash verify.sh space subclass POC1B_NONACTIVATING=0
#           bash verify.sh space subclass POC1B_SHOW=winit
#           bash verify.sh space plain
#           bash verify.sh space none     # 对照组（预期「不可见」）
#
# 判据（space 用例）：
#   1. 全屏 Space 激活期：onscreen 清单**无**本面板（隐藏态基线）
#   2. 触发 show 后：onscreen 清单**有**本面板 + 截图中面板压在全屏窗之上
#   3. hide 后：清单再次无本面板；show 后再次有（可见性双向）
#   4. quit：进程干净退出（无崩溃日志）
#
# 前置：窗口清单脚本 /tmp/winlist.swift（本脚本不存在时自动重建）。

set -u

BIN=./target/debug/poc1b-npanel
ACTOR=./target/debug/poc1b-fs-actor
LOG=/tmp/poc1b-run.log
ACTOR_LOG=/tmp/poc1b-actor.log
CMD=/tmp/poc1b.cmd
ACTOR_QUIT=/tmp/poc1b-actor-quit
WINLIST=/tmp/winlist.swift

# 窗口清单脚本（CoreGraphics，layer/onscreen/pid/owner/bounds 五列）。
if [[ ! -f "$WINLIST" ]]; then
  cat > "$WINLIST" <<'SWIFT'
import CoreGraphics
import Foundation
let mode = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "onscreen"
let opts: CGWindowListOption = (mode == "all")
    ? [.optionAll, .excludeDesktopElements]
    : [.optionOnScreenOnly, .excludeDesktopElements]
guard let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    print("no window list"); exit(1)
}
for w in list {
    let owner = w[kCGWindowOwnerName as String] as? String ?? "?"
    let name = w[kCGWindowName as String] as? String ?? ""
    let layer = w[kCGWindowLayer as String] as? Int ?? -999
    let pid = w[kCGWindowOwnerPID as String] as? Int ?? -1
    let onscreen = (w[kCGWindowIsOnscreen as String] as? Bool) ?? false
    let bounds = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    let x = (bounds["X"] as? Double) ?? -1
    let y = (bounds["Y"] as? Double) ?? -1
    let wd = (bounds["Width"] as? Double) ?? -1
    let ht = (bounds["Height"] as? Double) ?? -1
    let tag = name.isEmpty ? "" : " / \(name)"
    print("layer=\(layer) onscreen=\(onscreen) pid=\(pid) \(owner)\(tag) bounds=(\(Int(x)),\(Int(y)) \(Int(wd))x\(Int(ht)))")
}
SWIFT
  echo "[verify] 已重建 $WINLIST"
fi

panel_windows() { swift "$WINLIST" onscreen | grep -E "poc1b-npanel" || true; }
actor_windows() { swift "$WINLIST" onscreen | grep -E "poc1b-fs-actor" || true; }
panel_line() { local w; w=$(panel_windows); echo "${w:-(无)}"; }

cmd_file() { echo "$1" > "$CMD"; }

shot() { # $1 = 标签
  local tag="$1"
  screencapture -x -D 1 "/tmp/poc1b-$tag.png" 2>/dev/null \
    && echo "[verify] 截图 = /tmp/poc1b-$tag.png" \
    || echo "[verify] 截图失败（Screen Recording 权限？）"
}

cleanup() {
  touch "$ACTOR_QUIT" 2>/dev/null || true
  pkill -f poc1b-npanel 2>/dev/null || true
  pkill -f poc1b-fs-actor 2>/dev/null || true
  rm -f "$CMD" "$ACTOR_QUIT"
}
trap cleanup EXIT

case_smoke() {
  rm -f "$LOG" "$CMD"
  echo "=== smoke：桌面单跑（mode=subclass，直接可见）==="
  POC1B_START_HIDDEN=0 POC1B_POS=200,200 "$BIN" > "$LOG" 2>&1 &
  sleep 5
  echo "--- 启动日志 ---"; cat "$LOG"
  echo "--- onscreen 清单（本面板行）---"; panel_windows || echo "(无 —— 异常)"
  shot "smoke"
}

# 输入链路差分：合成按键能否进 TextEdit。
# 该方法依赖 app 处于激活态（AppKit 丢弃非激活 app 的键盘事件），故先 activate。
# 差分要点：mode=subclass 与 mode=none 两例结果一致 ⇒ 换类未破坏输入链路。
run_input_case() { # $1 = mode, $2 = tag
  local mode="$1" tag="$2"
  local log="/tmp/poc1b-input-$tag.log"
  rm -f "$log" "$CMD"
  POC1B_MODE="$mode" POC1B_START_HIDDEN=0 POC1B_POS=200,200 "$BIN" > "$log" 2>&1 &
  local pid=$!
  sleep 4
  cmd_file activate; sleep 1
  cmd_file key;      sleep 1
  cmd_file focus;    sleep 1
  cmd_file type;     sleep 2
  local received
  received=$(grep -c "TextEdit 收到输入" "$log" || true)
  grep -E "activate：|makeKeyWindow：|已投递合成按键|TextEdit 收到输入" "$log" || true
  echo "[input-diff] mode=${mode}：TextEdit 收到输入次数 = ${received}（日志 ${log}）"
  kill "$pid" 2>/dev/null || true
  sleep 1
}

case_input_diff() {
  echo "=== input-diff：合成按键 → TextEdit（subclass / plain / none 三例对照）==="
  run_input_case subclass subclass
  run_input_case plain plain
  run_input_case none none
}

# 跨全屏 Space 主测。
case_space() { # $1 = mode，其余 = 追加 env（如 POC1B_NONACTIVATING=0）
  local mode="${1:-subclass}"; shift || true
  local tag="$mode" extra
  for extra in "$@"; do tag="${tag}_$(echo "$extra" | tr '=' '-')"; done
  local log="/tmp/poc1b-space-$tag.log"
  rm -f "$log" "$ACTOR_LOG" "$CMD" "$ACTOR_QUIT"
  echo "=== space[$tag]：mode=$mode 追加 env = $* ==="

  echo "--- 1) 起面板（默认隐藏启动）---"
  env POC1B_MODE="$mode" POC1B_POS=200,200 "$@" "$BIN" > "$log" 2>&1 &
  local panel_pid=$!
  sleep 3
  grep -E "配置|换类回读" "$log" || true

  echo "--- 2) 起全屏角色（他人应用进程）---"
  "$ACTOR" > "$ACTOR_LOG" 2>&1 &
  sleep 7
  echo "--- 3) 全屏 Space 基线清单 ---"
  local base_panel base_actor
  base_panel=$(panel_windows); base_actor=$(actor_windows)
  echo "面板（应为空）：${base_panel:-(无)}"
  echo "全屏角色：${base_actor:-(无 —— 异常，全屏未生效)}"
  shot "space-$tag-baseline"

  echo "--- 4) 触发 show ---"
  cmd_file show
  sleep 2
  local after_panel
  after_panel=$(panel_windows)
  if [[ -n "$after_panel" ]]; then
    echo "✅ 面板出现在当前（全屏）Space 的 onscreen 清单：$after_panel"
  else
    echo "❌ 面板不在当前 Space（跨全屏 Space 失败）"
  fi
  echo "全屏角色：$(actor_windows)"
  shot "space-$tag-shown"

  echo "--- 5) key + type（合成按键自测）---"
  cmd_file key; sleep 1
  cmd_file type; sleep 2
  grep -E "makeKeyWindow：|已投递合成按键|TextEdit 收到输入" "$log" || true

  echo "--- 6) hide → 清单应转空；show → 清单应回 ---"
  cmd_file hide; sleep 1
  echo "hide 后面板：$(panel_line)"
  cmd_file show; sleep 2
  echo "show 后面板：$(panel_line)"

  echo "--- 7) quit（应干净退出，无崩溃）---"
  cmd_file quit
  sleep 3
  if kill -0 "$panel_pid" 2>/dev/null; then
    echo "❌ 进程仍在（quit 未生效）"
  elif grep -qE "panicked at|non-unwinding panic" "$log"; then
    echo "❌ 退出时崩溃（panic/abort）："
    grep -E "panicked at|non-unwinding|aborting" "$log" | tail -3
  else
    echo "✅ 进程已干净退出；日志尾："
    tail -3 "$log"
  fi
  echo "--- 8) 收尾：退出全屏角色 ---"
  touch "$ACTOR_QUIT"
  sleep 3
  pkill -f poc1b-fs-actor 2>/dev/null || true
  echo "=== space[${tag}] 完成：日志 ${log}；截图 /tmp/poc1b-space-${tag}-*.png ==="
}

case_follow() { # 面板先可见（桌面）→ 角色再全屏：看面板是否跟随进全屏 Space
  local log="/tmp/poc1b-follow.log"
  rm -f "$log" "$ACTOR_LOG" "$CMD" "$ACTOR_QUIT"
  echo "=== follow：面板先可见，再让他人应用进全屏 ==="
  POC1B_MODE=subclass POC1B_START_HIDDEN=0 POC1B_POS=200,200 "$BIN" > "$log" 2>&1 &
  sleep 3
  echo "桌面基线（应有面板）: $(panel_windows)"
  "$ACTOR" > "$ACTOR_LOG" 2>&1 &
  sleep 7
  echo "全屏 Space 激活期面板：$(panel_windows || echo '(无)')"
  echo "全屏角色：$(actor_windows)"
  shot "follow-in-fs"
  touch "$ACTOR_QUIT"
  sleep 3
}

case_hotkey_hide_show() { # 在「他人全屏 Space 里」验证 hide/show 双向（非激活面板不留焦点）
  local log="/tmp/poc1b-hotkey.log"
  rm -f "$log" "$ACTOR_LOG" "$CMD" "$ACTOR_QUIT"
  echo "=== hotkey-hide-show：全屏 Space 内 hide → show 双向 ==="
  POC1B_MODE=subclass POC1B_POS=200,200 "$BIN" > "$log" 2>&1 &
  sleep 3
  "$ACTOR" > "$ACTOR_LOG" 2>&1 &
  sleep 7
  cmd_file show; sleep 2
  echo "show 后：$(panel_windows || echo '(无)')"
  cmd_file hide; sleep 1
  echo "hide 后：$(panel_windows || echo '(无)')"
  cmd_file show; sleep 2
  echo "再 show 后：$(panel_windows || echo '(无)')"
  shot "hotkey-after-show"
  touch "$ACTOR_QUIT"; sleep 3
}

main() {
  local case_name="${1:-}"
  shift || true
  case "$case_name" in
    smoke) case_smoke ;;
    input-diff) case_input_diff ;;
    space) case_space "$@" ;;
    follow) case_follow ;;
    hotkey-hide-show) case_hotkey_hide_show ;;
    *) sed -n '2,20p' "$0"; exit 1 ;;
  esac
}

main "$@"
