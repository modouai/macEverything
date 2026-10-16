#!/usr/bin/env bash
# PoC-5 三态验证（全自动）：初始态 → register → 观察 → unregister → 观察 → 复原注册
# 用法: bash verify.sh <direct|open> [app路径]
#   direct = 直接执行 $APP/Contents/MacOS/poc5-binary（不经 LaunchServices）
#   open   = open $APP --args ...（经 LaunchServices 启动 bundle）
# 证据：stdout + 日志文件增量（默认 /tmp/poc5.log）+ launchctl / sfltool dumpbtm 观察
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="${2:-${POC5_APP:-/tmp/poc5.app}}"
BIN="$APP/Contents/MacOS/poc5-binary"
LOG="${POC5_LOG:-/tmp/poc5.log}"
MODE="${1:?用法: verify.sh <direct|open> [app路径]}"
UID_N="$(id -u)"
touch "$LOG" 2>/dev/null || true

echo "########## PoC-5 三态验证：MODE=$MODE APP=$APP ##########"

# 看门狗：macOS 无 timeout；超时 kill -9 并返回 124
run_with_watchdog() {  # 用法: run_with_watchdog <秒> <命令...>
    local secs="$1"; shift
    "$@" & local p=$!
    ( sleep "$secs"; kill -9 "$p" 2>/dev/null ) & local w=$!
    wait "$p" 2>/dev/null; local rc=$?
    kill -9 "$w" 2>/dev/null
    wait "$w" 2>/dev/null
    [[ $rc -ge 128 ]] && return 124
    return $rc
}

invoke() {
    local before after
    before=$(wc -l < "$LOG" 2>/dev/null || echo 0)
    echo "--- invoke[$MODE]: $* ---"
    if [[ "$MODE" == "open" ]]; then
        open "$APP" --args "$@"
        sleep 2   # open 异步返回：等 app 进程真正跑完并写日志
    else
        "$BIN" "$@"
    fi
    after=$(wc -l < "$LOG" 2>/dev/null || echo 0)
    echo "--- 日志增量（$LOG 第 $((before+1))..$after 行）---"
    tail -n +$((before + 1)) "$LOG" 2>/dev/null || echo "(无日志)"
}

obs() {
    local tag="$1"
    echo "--- 观察[$tag]: launchctl list | grep -i poc5 ---"
    launchctl list 2>&1 | grep -i poc5 || echo "(无匹配)"
    echo "--- 观察[$tag]: launchctl print gui/$UID_N | grep -i poc5 ---"
    launchctl print "gui/$UID_N" 2>&1 | grep -i poc5 || echo "(无匹配)"
    # BTM 数据库（sfltool dumpbtm）会走 AuthorizationCopyRights：本机会挂起等授权，故默认跳过；
    # 置 POC5_BTM=1 时带看门狗尝试（8s 超时），结果如实打印。
    if [[ "${POC5_BTM:-0}" == "1" ]]; then
        echo "--- 观察[$tag]: sfltool dumpbtm（BTM 数据库，看门狗 8s）| grep -i poc5 ---"
        if run_with_watchdog 8 sfltool dumpbtm >/tmp/poc5-btm.txt 2>/tmp/poc5-btm.err; then
            grep -i poc5 /tmp/poc5-btm.txt || echo "(BTM 无 poc5 条目)"
        else
            echo "(sfltool dumpbtm 未完成 rc=${?}（挂起于 AuthorizationCopyRights 等授权）；stderr: $(head -2 /tmp/poc5-btm.err | tr '\n' ' '))"
        fi
    else
        echo "--- 观察[$tag]: sfltool dumpbtm 跳过（默认；POC5_BTM=1 启用）---"
    fi
}

echo "===== [1] 初始状态（预期 notRegistered=0；LS 未认识的新 app 可能为 notFound=3）====="
invoke status
obs "初始"

echo "===== [2] register ====="
invoke register
invoke status

echo "===== [3] 注册后外部观察（launchctl 预期不可见—BTM 管理不落 launchd 服务）====="
obs "已注册"

echo "===== [4] daemon 常驻 6s（ps 观察 2s，供「登录后 ps 可见」参考）====="
if [[ "$MODE" == "direct" ]]; then
    "$BIN" daemon 6 &
    DPID=$!
    sleep 2
    echo "--- ps aux | grep poc5-binary（daemon 子进程 pid=${DPID}）---"
    ps aux | grep -i "[p]oc5-binary" || echo "(无匹配)"
    echo "--- launchctl 视图中的 poc5（daemon 存活期）---"
    launchctl print "gui/$UID_N" 2>&1 | grep -i poc5 || echo "(无匹配)"
    wait "$DPID" 2>/dev/null
else
    echo "(open 模式跳过 daemon 常驻：open 启动的实例无法在本脚本回收，改由人工/后续步骤观察)"
fi

echo "===== [5] unregister ====="
invoke unregister
invoke status

echo "===== [6] 注销后外部观察（应无残留）====="
obs "已注销"

echo "===== [7] 复原：再 register（终态=已启用，留作重启验证）====="
invoke register
invoke status
obs "复原后"

echo "########## PoC-5 验证结束：MODE=$MODE ##########"
