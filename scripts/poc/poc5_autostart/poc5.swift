// PoC-5: SMAppService 自启（ad-hoc 签名）— 注册 / 查询 / 注销
//
// 关键约束：SMAppService.mainApp 的身份由 Bundle.main 决定，故须在 .app bundle 内调用（实测 bundle 外不报错但会注册出独立条目）。
// 编译：swiftc poc5.swift -o poc5-binary -framework ServiceManagement
// 组装：见同目录 make-mini-app.sh（组最小 .app + ad-hoc 签名）
//
// 用法（在 bundle 内执行，或经 open 传入 --args）：
//   poc5-binary register            注册登录项
//   poc5-binary status              查询状态（0=notRegistered 1=enabled 2=requiresApproval 3=notFound）
//   poc5-binary unregister          注销登录项
//   poc5-binary daemon [秒数]       常驻（默认 600s）——登录项拉起时无参数，默认走 daemon 便于 ps 观察
//   poc5-binary （无参数）          同 daemon 600（模拟登录项拉起的默认行为）
//
// 输出：stdout + 追加写日志文件（POC5_LOG 覆盖，默认 /tmp/poc5.log）。
// 经 `open` 启动时 stdout 不可见，日志文件是唯一取证渠道。

import Foundation
import ServiceManagement
import Darwin

let logPath = ProcessInfo.processInfo.environment["POC5_LOG"] ?? "/tmp/poc5.log"

func log(_ msg: String) {
    let ts = ISO8601DateFormatter().string(from: Date())
    let line = "[\(ts)] pid=\(getpid()) ppid=\(getppid()) \(msg)\n"
    let data = Data(line.utf8)
    FileHandle.standardOutput.write(data)
    if let fh = try? FileHandle(forWritingTo: URL(fileURLWithPath: logPath)) {
        fh.seekToEndOfFile()
        fh.write(data)
        try? fh.close()
    } else {
        try? data.write(to: URL(fileURLWithPath: logPath))
    }
}

func describe(_ status: SMAppService.Status) -> String {
    switch status {
    case .notRegistered: return "notRegistered(0)"
    case .enabled: return "enabled(1)"
    case .requiresApproval: return "requiresApproval(2)"
    case .notFound: return "notFound(3)"
    @unknown default: return "unknown(\(status.rawValue))"
    }
}

let args = Array(CommandLine.arguments.dropFirst())
let cmd = args.first ?? "<none>"

// 启动上下文（bundle 识别与启动方式差异的取证点）
log("起: cmd=\(cmd) argv=[\(args.joined(separator: " "))] bundleID=\(Bundle.main.bundleIdentifier ?? "<nil>") bundlePath=\(Bundle.main.bundlePath) exec=\(Bundle.main.executablePath ?? "<nil>")")

let svc = SMAppService.mainApp

switch cmd {
case "register":
    do {
        try svc.register()
        log("register: OK（无抛错）→ status=\(describe(svc.status))")
    } catch {
        let ns = error as NSError
        log("register: ERROR domain=\(ns.domain) code=\(ns.code) desc=\(ns.localizedDescription) raw=\(error)")
    }

case "status":
    log("status=\(describe(svc.status)) raw=\(svc.status.rawValue)")

case "unregister":
    do {
        try svc.unregister()
        log("unregister: OK（无抛错）→ status=\(describe(svc.status))")
    } catch {
        let ns = error as NSError
        log("unregister: ERROR domain=\(ns.domain) code=\(ns.code) desc=\(ns.localizedDescription) raw=\(error)")
    }

case "daemon":
    let secs = args.count > 1 ? (Double(args[1]) ?? 600) : 600
    log("daemon: 进入常驻 \(secs)s（ps aux | grep poc5-binary 可观察）；当前 status=\(describe(svc.status))")
    Thread.sleep(forTimeInterval: secs)
    log("daemon: 常驻结束退出")

default:
    // 登录项由 SMAppService 拉起时（预期）不带参数 → 默认走 daemon，便于登录后观察进程
    log("无参数：按「登录项拉起」处理，进入 daemon 600s；当前 status=\(describe(svc.status))")
    Thread.sleep(forTimeInterval: 600)
    log("daemon: 常驻结束退出")
}
