//! App 单实例:app.sock 通道(product §5,与 daemon.sock 并列于数据目录)。
//!
//! 启动时 bind app.sock:
//! - 成功 → 本进程为唯一实例;监听线程读一行命令,"show" 经 mpsc 投递给 UI
//!   主循环(并唤醒空闲的 egui 事件循环);
//! - 绑定失败且可连 → 已有实例:发 "show" 后 process::exit(0)——兑现
//!   「二次启动唤起悬浮条」;
//! - 绑定失败且拒连 → 陈旧 socket(上次异常退出残留):清理重绑。
//!
//! 监听线程随进程退出(daemon 线程),无需优雅停止;残留 socket 由下次启动
//! 的陈旧分支清理。

use anyhow::{Context, Result};
use client::DaemonPaths;
use eframe::egui;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

/// 二次启动唤起事件(监听线程 → UI 主循环)。
pub enum Incoming {
    /// 显示悬浮条。
    ShowPalette,
}

/// 单实例持有凭据:socket 由监听线程持有,本结构只作 UI 唤醒注入入口
/// (attach_ui 后即可丢弃,线程与 socket 随进程存活)。
pub struct SingleInstance {
    /// UI 唤醒槽:App 建好 egui Context 后注入(见 attach_ui)。
    waker: Arc<OnceLock<egui::Context>>,
    _listen: std::thread::JoinHandle<()>,
}

impl SingleInstance {
    /// 注入 egui Context:监听线程投递事件后 request_repaint(官方跨线程 API),
    /// 空闲(无输入事件)时主循环也会醒来处理;未注入时事件滞留到下次重绘。
    pub fn attach_ui(&self, ctx: egui::Context) {
        let _ = self.waker.set(ctx);
    }
}

/// 获取单实例权。已有实例在跑时:发唤起命令后 process::exit(0)(不返回)。
pub fn acquire(paths: &DaemonPaths) -> Result<(SingleInstance, mpsc::Receiver<Incoming>)> {
    let sock = paths.data_dir.join("app.sock");
    std::fs::create_dir_all(&paths.data_dir).context("创建数据目录失败")?;
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match UnixStream::connect(&sock) {
            // 已有实例在监听:发唤起命令,本进程退出(退出码 0)
            Ok(mut s) => {
                let _ = s.write_all(b"show\n");
                let _ = s.flush();
                log::info!("已有实例在运行(app.sock 可连),发送唤起命令后退出");
                std::process::exit(0);
            }
            // 拒连 = 陈旧 socket(上次异常退出残留):清理重绑
            Err(_) => {
                log::warn!("app.sock 陈旧(无可连实例),清理后重绑");
                std::fs::remove_file(&sock).context("清理陈旧 app.sock 失败")?;
                UnixListener::bind(&sock).context("重绑 app.sock 失败")?
            }
        },
        Err(e) => return Err(e).context("绑定 app.sock 失败"),
    };
    // socket 权限 0600(仅本用户可连;与 daemon.sock 同规)
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));

    let (tx, rx) = mpsc::channel();
    let waker: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let thread_waker = Arc::clone(&waker);
    let listen = std::thread::Builder::new()
        .name("app-sock".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if handle_conn(stream, &tx) {
                    // 事件已投递:唤醒 UI 主循环(空闲时 egui 不跑 logic)
                    if let Some(ctx) = thread_waker.get() {
                        ctx.request_repaint();
                    }
                }
            }
        })
        .context("启动 app.sock 监听线程失败")?;
    Ok((
        SingleInstance {
            waker,
            _listen: listen,
        },
        rx,
    ))
}

/// 处理一条连接:读一行命令。返回 true = 事件已投递。
/// "show" → Incoming::ShowPalette;空行(探测)/ 未知命令 → 忽略并留痕。
fn handle_conn(stream: UnixStream, tx: &mpsc::Sender<Incoming>) -> bool {
    // 读超时:半开连接(连上不发)不拖住 accept 循环
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    if reader.read_line(&mut line).is_err() {
        return false; // 超时/半开:无命令,忽略
    }
    match line.trim() {
        "show" => tx.send(Incoming::ShowPalette).is_ok(),
        "" => false,
        other => {
            log::warn!("app.sock 未知命令: {other}");
            false
        }
    }
}
