//! 空闲死页归还(内存规格 `docs/specs/memory-footprint/tech.md` §6.2.2)。
//!
//! ## 机制
//!
//! GUI 交互期(tessellation、galley 重排、图标上传)产生大量「分配-释放」,
//! macOS libmalloc 把归还的大块脏页滞留为 `Malloc Large (empty)`(峰值时实测
//! 3GB/362 块滞留),不会主动还给内核。`engine::malloc_pressure::release_heap_pressure`
//! (`malloc_zone_pressure_relief`)强制折叠。daemon 侧已在重扫/落盘后调用;
//! GUI 侧没有天然的「事办完了」时机,故按**输入静默**判定:最后一个视口输入
//! 事件(键/指针/滚轮)之后 60s 仍无输入,归还一次;再有输入后重新计时。
//!
//! ## 归属判定口径
//!
//! 「空闲」以**视口级输入静默**为准,不是「没有帧」——悬浮条常驻,主题跟随、
//! 图标在途轮询等都会排帧,这些帧不代表用户在场,不重置计时。四个视口
//! (根窗/悬浮条/设置/浮层)各自在绘制入口调 [`note_viewport_activity`],任一有输入
//! 即视为活跃。
//!
//! ## 执行线程
//!
//! 归还由专用看门狗线程执行,不走帧——产品形态四窗皆隐时排帧完全停止
//! (L4 隐藏零帧红线),挂在帧上永远轮不到归还。`malloc_zone_pressure_relief`
//! 线程无关,任何线程可调。轮询间隔 15s:最坏 75s 归还,换来空闲期 CPU 归零。

use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use eframe::egui;

use engine::malloc_pressure::release_heap_pressure;

/// 输入静默多久后归还一次。
pub(crate) const IDLE_AFTER: Duration = Duration::from_secs(60);
/// 看门狗轮询间隔(最坏归还延迟 = IDLE_AFTER + POLL)。
const POLL: Duration = Duration::from_secs(15);

/// 空闲归还的判定状态机(纯逻辑,单测直接喂时间)。
///
/// 语义:
/// - 无任何活跃记录(理论上仅直接构造时):不归还;
/// - 距上次活跃 ≥ [`IDLE_AFTER`] 且**本次静默段尚未归还过** → 归还;
/// - 「本次静默段已归还过」= 上次归还时间不早于上次活跃时间——归还本身
///   不重置活跃,同一静默段内不重复归还;新活跃到达后重新具备资格。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IdleGate {
    last_activity: Option<Instant>,
    last_release: Option<Instant>,
}

impl IdleGate {
    /// 起点即视为活跃:启动期的临时分配(字体解析/引擎拉起/图标线程)在
    /// 开机后无输入的常驻形态下也能在 60s 后被归还。
    pub(crate) fn starting_at(now: Instant) -> Self {
        Self {
            last_activity: Some(now),
            last_release: None,
        }
    }

    /// 记录一次视口输入(四视口任一有输入事件即调)。
    pub(crate) fn note_activity(&mut self, now: Instant) {
        self.last_activity = Some(now);
    }

    /// 到点判定。返回 true 表示**调用方应当执行归还**(本方法只改状态机,
    /// 不做系统调用——归还失败/开销与判定无关)。
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_activity else {
            return false;
        };
        if now.duration_since(last) < IDLE_AFTER {
            return false;
        }
        if self.last_release.is_some_and(|r| r >= last) {
            return false;
        }
        self.last_release = Some(now);
        true
    }
}

/// 全局状态机(单实例;UI 线程写活跃、看门狗线程读到点判定)。
static GATE: OnceLock<Mutex<IdleGate>> = OnceLock::new();

fn gate() -> &'static Mutex<IdleGate> {
    GATE.get_or_init(|| Mutex::new(IdleGate::starting_at(Instant::now())))
}

/// 四视口绘制入口共用的活跃采样:本帧有输入事件(键/指针/滚轮/IME)才算
/// 用户在场。纯排帧(主题跟随、图标轮询、动画尾巴)不算。
pub(crate) fn note_viewport_activity(ctx: &egui::Context) {
    let had_input = ctx.input(|i| !i.events.is_empty());
    if had_input {
        gate()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .note_activity(Instant::now());
    }
}

/// 启动看门狗线程(App::new 接线;失败仅留痕,不阻塞启动)。
pub(crate) fn spawn_watchdog() {
    let spawned = std::thread::Builder::new()
        .name("idle-pressure".to_owned())
        .spawn(|| loop {
            std::thread::sleep(POLL);
            let due = gate()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .due(Instant::now());
            if due {
                release_heap_pressure();
                log::info!("空闲 {}s:归还 malloc 死页", IDLE_AFTER.as_secs());
            }
        });
    if let Err(err) = spawned {
        log::warn!("空闲归还看门狗线程启动失败:{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// 起点即活跃:启动后静默 60s 内不归还,到点归还一次。
    #[test]
    fn starting_gate_releases_once_after_idle_threshold() {
        let t0 = Instant::now();
        let mut gate = IdleGate::starting_at(t0);
        assert!(!gate.due(t0 + secs(59)), "静默不足 60s 不得归还");
        assert!(gate.due(t0 + secs(60)), "到点应归还");
        // 同一静默段不重复归还(看门狗 15s 轮询会多次到点)。
        assert!(!gate.due(t0 + secs(75)), "同一静默段只归还一次");
        assert!(!gate.due(t0 + secs(600)), "同一静默段只归还一次");
    }

    /// 新活跃重置计时;归还后若一直无新活跃,不再归还(修复的是滞留页,
    /// 反复归还无意义)。
    #[test]
    fn activity_resets_timer_and_requires_new_idle_period() {
        let t0 = Instant::now();
        let mut gate = IdleGate::starting_at(t0);
        assert!(gate.due(t0 + secs(61)));
        // 归还后立刻来了一次输入:t1 起重新计时。
        let t1 = t0 + secs(62);
        gate.note_activity(t1);
        assert!(!gate.due(t1 + secs(59)));
        assert!(gate.due(t1 + secs(60)), "新静默段到点应再次归还");
    }

    /// 交互中(每次输入间隔小于阈值)永不归还——悬浮条常驻交互频繁的形态。
    #[test]
    fn continuous_activity_never_releases() {
        let t0 = Instant::now();
        let mut gate = IdleGate::starting_at(t0);
        for i in 0..200u32 {
            let now = t0 + secs(i as u64 * 30); // 每 30s 一次输入
            gate.note_activity(now);
            assert!(!gate.due(now + secs(1)), "持续交互不得归还");
        }
    }

    /// 无活跃记录(人工构造)不归还;归还判定不依赖真实时钟。
    #[test]
    fn no_activity_never_releases() {
        let mut gate = IdleGate {
            last_activity: None,
            last_release: None,
        };
        assert!(!gate.due(Instant::now()));
    }

    /// 突变钉:阈值改成 59s 以下首测应红(阈值常量被有意调小可被发现)。
    /// 通过 60/59 边界双向断言实现:59s 归还 = 阈值泄漏。
    #[test]
    fn threshold_is_sixty_seconds() {
        assert_eq!(IDLE_AFTER, secs(60));
        let t0 = Instant::now();
        let mut gate = IdleGate::starting_at(t0);
        assert!(!gate.due(t0 + secs(59)));
        assert!(gate.due(t0 + secs(60)));
    }
}
