//! 查询会话:防抖 + 代际号 + 六态状态机(纯逻辑;时钟由调用方传入,测试可注入)。

use std::time::{Duration, Instant};

/// 六态(tech.md 表):Idle 空词不请求;Searching 请求中/有结果;EngineStarting 拉起中;
/// Offline 离线结果;Empty total=0;Warming 索引未挂载;Error 其余错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    Searching,
    EngineStarting,
    Offline,
    Empty,
    Warming,
    Error(String),
}

// 定案(v2 plan Task 11):直接复用 engine::query::SortKey
pub use engine::query::SortKey;

/// 防抖窗到期后应发起的查询(代际号供响应到达时校验)。
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub query: String,
    pub sort: SortKey,
    /// 方向:反转 `sort` 的自然方向(Task 19b;协议 desc 字段的客户端语义)。
    /// 由调用方经 `request_desc`(ui/results.rs)从界面方向换算后交给会话。
    pub desc: bool,
    pub generation: u64,
}

pub struct Session {
    debounce: Duration,
    /// 最近一次输入(串, 时刻);窗口内只保留最新
    pending: Option<(String, Instant)>,
    /// 最近一次已发起的查询(串, 排序);排序变更时按此串重发
    last_fired: Option<(String, SortKey)>,
    sort: SortKey,
    /// 当前方向(反转自然方向);随排序变更原子更新(键与方向同属一次请求)
    desc: bool,
    generation: u64,
    pub state: State,
}

impl Session {
    pub fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            pending: None,
            last_fired: None,
            sort: SortKey::Relevance,
            desc: false,
            generation: 0,
            state: State::Idle,
        }
    }

    /// 输入变更:更新 pending(窗口内只保留最新);空词 → 清 pending/last_fired + State::Idle
    /// (last_fired 同清:排序变更不得复活空查询)并**推进代际号作废在途响应**
    /// (清空输入后旧代结果不得回填)。
    pub fn on_input(&mut self, text: &str, now: Instant) {
        if text.is_empty() {
            self.pending = None;
            self.last_fired = None;
            self.generation += 1; // 在途响应作废(Task 12 清空即清结果的语义)
            self.state = State::Idle;
        } else {
            self.pending = Some((text.to_string(), now));
        }
    }

    /// 排序变更:同串重发 —— 窗内有 pending 保持原串、刷新时刻;无 pending 按
    /// last_fired 的串构造 pending(代际号在下一次 pending_at 自然 +1);
    /// 两者皆无(空词/从未发起)→ 不动。
    /// `desc` = 协议方向(反转自然方向),与键原子更新:方向变了必须重查——
    /// 引擎按方向排序后截断,截断集不能靠旧响应本地反转(Task 19b)。
    /// 消费方:完整窗口点列头排序(ui/results.rs);悬浮条不用此能力(product §8)。
    pub fn on_sort_change(&mut self, key: SortKey, desc: bool, now: Instant) {
        self.sort = key;
        self.desc = desc;
        let query = self
            .pending
            .as_ref()
            .map(|(q, _)| q.clone())
            .or_else(|| self.last_fired.as_ref().map(|(q, _)| q.clone()));
        if let Some(query) = query {
            self.pending = Some((query, now));
        }
    }

    /// 隐藏项开关变化：同串重发（语义同排序变更，但不改排序/方向）。
    /// 窗内有 pending 保持原串、刷新时刻；无 pending 按 last_fired 的串构造；
    /// 两者皆无（空词/从未发起）→ 不动。
    #[allow(dead_code)] // Task 8 设置页开关写入成功后调用两视口会话。
    pub fn on_hidden_change(&mut self, now: Instant) {
        let query = self
            .pending
            .as_ref()
            .map(|(q, _)| q.clone())
            .or_else(|| self.last_fired.as_ref().map(|(q, _)| q.clone()));
        if let Some(query) = query {
            self.pending = Some((query, now));
        }
    }

    /// 复位会话(关闭/重新打开浮层等):清 pending/last_fired,推进代际号作废在途
    /// 响应,状态回 Idle。排序/方向/防抖窗保持。
    pub fn reset(&mut self) {
        self.pending = None;
        self.last_fired = None;
        self.generation += 1;
        self.state = State::Idle;
    }

    /// 窗口到期(自最近一次输入/排序变更起算 debounce)且有待发 → 发起
    /// (gen+=1, State=Searching);否则 None。
    pub fn pending_at(&mut self, now: Instant) -> Option<Fire> {
        let (query, at) = self.pending.as_ref()?;
        if now < *at + self.debounce {
            return None;
        }
        let query = query.clone();
        self.pending = None;
        self.generation += 1;
        self.state = State::Searching;
        self.last_fired = Some((query.clone(), self.sort));
        Some(Fire {
            query,
            sort: self.sort,
            desc: self.desc,
            generation: self.generation,
        })
    }

    /// 代际校验:当代(= 最近一次 pending_at 发出的代)接受,旧代丢弃。
    pub fn accept_response(&mut self, generation: u64) -> bool {
        self.accepts(generation)
    }

    /// 代际校验(只读):generation 是否为当前代。
    pub fn accepts(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// 防抖窗口剩余时长(Some = 有查询在途未发)。调用方**每帧**据此补排 repaint:
    /// egui 的排帧合并只触发「本帧最短的那次请求」(`context.rs:167` —
    /// `delay < repaint_delay` 才回调),输入帧常伴 0ms 立即重绘请求,会把同一帧里
    /// 更晚的 60ms 防抖请求吞掉;只排一次的实现会一直等到下一次偶然重绘
    /// (实测:光标闪烁,~500ms)才发查询——「展开列表慢」的根因。
    pub fn pending_delay(&self, now: Instant) -> Option<Duration> {
        let (_, at) = self.pending.as_ref()?;
        Some((*at + self.debounce).saturating_duration_since(now))
    }

    /// 响应错误码 → 状态:"warming"(索引未挂载)→ Warming;其余 → Error(code)。
    pub fn mark_error(&mut self, code: &str) {
        self.state = if code == "warming" {
            State::Warming
        } else {
            State::Error(code.to_string())
        };
    }

    /// 响应 total → 状态:0 → Empty(无匹配项);>0 → Searching(有结果)。
    pub fn mark_total(&mut self, total: u32) {
        self.state = if total == 0 {
            State::Empty
        } else {
            State::Searching
        };
    }

    pub fn mark_engine_starting(&mut self) {
        self.state = State::EngineStarting;
    }

    pub fn mark_offline(&mut self) {
        self.state = State::Offline;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn debounce_coalesces_keystrokes() {
        let mut s = Session::new(Duration::from_millis(60));
        let base = Instant::now();
        s.on_input("a", base);
        s.on_input("ab", base + Duration::from_millis(20));
        s.on_input("abc", base + Duration::from_millis(40));
        assert_eq!(
            s.pending_at(base + Duration::from_millis(80)),
            None,
            "窗口未到"
        );
        let fire = s
            .pending_at(base + Duration::from_millis(110))
            .expect("窗口到应发起");
        assert_eq!(fire.query, "abc");
        assert_eq!(fire.generation, 1, "三次输入合并为一代");
    }

    #[test]
    fn stale_response_discarded() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("a", base);
        let g1 = s
            .pending_at(base + Duration::from_millis(20))
            .unwrap()
            .generation;
        s.on_input("ab", base + Duration::from_millis(30));
        let g2 = s
            .pending_at(base + Duration::from_millis(50))
            .unwrap()
            .generation;
        assert!(g2 > g1);
        assert!(!s.accept_response(g1), "旧代响应丢弃");
        assert!(s.accept_response(g2), "当代响应接受");
    }

    /// `accepts`:当代接受、旧代拒绝(只读,不推进代际号)。
    #[test]
    fn accepts_only_current_generation() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("a", base);
        let g1 = s
            .pending_at(base + Duration::from_millis(20))
            .expect("窗口到应发起")
            .generation;
        assert!(s.accepts(g1), "当代接受");
        assert!(!s.accepts(g1.wrapping_sub(1)), "旧代拒绝");
        s.on_input("b", base + Duration::from_millis(30));
        let g2 = s
            .pending_at(base + Duration::from_millis(50))
            .expect("窗口到应发起")
            .generation;
        assert!(s.accepts(g2));
        assert!(!s.accepts(g1), "新代发出后旧代拒绝");
    }

    /// `reset`:清 pending/last_fired、推进代际号作废在途、状态回 Idle。
    #[test]
    fn reset_invalidates_inflight_and_idles() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("a", base);
        let g = s
            .pending_at(base + Duration::from_millis(20))
            .expect("窗口到应发起")
            .generation;
        assert!(s.accepts(g));
        s.reset();
        assert!(!s.accepts(g), "reset 后在途旧代不被接受");
        assert!(matches!(s.state, State::Idle), "状态回 Idle");
        assert_eq!(s.pending_delay(base), None, "pending 清空");
        assert_eq!(
            s.pending_at(base + Duration::from_secs(1)),
            None,
            "last_fired/pending 清空后不复活查询"
        );
    }

    #[test]
    fn empty_query_idles() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("", base);
        assert_eq!(
            s.pending_at(base + Duration::from_millis(50)),
            None,
            "空词不发起"
        );
        assert!(matches!(s.state, State::Idle));
    }

    #[test]
    fn clearing_input_discards_inflight_response() {
        // 输入 → 发起(gen 1)→ 清空输入:在途响应到达时必须被作废
        // (清空即清结果:旧代结果不得回填)。
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("报告", base);
        let g = s
            .pending_at(base + Duration::from_millis(20))
            .expect("窗口到应发起")
            .generation;
        s.on_input("", base + Duration::from_millis(30));
        assert!(!s.accept_response(g), "清空后旧代响应丢弃");
    }

    /// 防抖剩余时长(逐帧补排 repaint 用):无在途 → None;随帧收缩;到期/超期 → 0
    /// (立即重绘);发起后/空词清空后 → None。
    #[test]
    fn pending_delay_tracks_debounce_window() {
        let mut s = Session::new(Duration::from_millis(60));
        let base = Instant::now();
        assert_eq!(s.pending_delay(base), None, "无在途查询");
        s.on_input("a", base);
        assert_eq!(s.pending_delay(base), Some(Duration::from_millis(60)));
        assert_eq!(
            s.pending_delay(base + Duration::from_millis(40)),
            Some(Duration::from_millis(20)),
            "剩余时长随帧推进收缩"
        );
        assert_eq!(
            s.pending_delay(base + Duration::from_millis(60)),
            Some(Duration::ZERO),
            "到期为 0"
        );
        assert_eq!(
            s.pending_delay(base + Duration::from_secs(5)),
            Some(Duration::ZERO),
            "超期饱和为 0,不 panic"
        );
        let _ = s.pending_at(base + Duration::from_millis(60));
        assert_eq!(
            s.pending_delay(base + Duration::from_millis(60)),
            None,
            "发起后清空"
        );
        s.on_input("b", base);
        s.on_input("", base);
        assert_eq!(s.pending_delay(base), None, "空词清 pending");
    }

    #[test]
    fn warming_and_error_states() {
        let mut s = Session::new(Duration::from_millis(10));
        s.mark_error("warming");
        assert!(matches!(s.state, State::Warming));
        s.mark_error("parse");
        assert!(matches!(s.state, State::Error(_)));
    }

    #[test]
    fn sort_change_refires_same_query() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = Instant::now();
        s.on_input("a", base);
        let f1 = s.pending_at(base + Duration::from_millis(20)).unwrap();
        assert!(!f1.desc, "缺省方向 = 自然方向");
        s.on_sort_change(SortKey::Size, true, base + Duration::from_millis(30));
        let f2 = s.pending_at(base + Duration::from_millis(50)).unwrap();
        assert_eq!(f2.query, f1.query);
        assert_ne!(f2.generation, f1.generation);
        assert_eq!(f2.sort, SortKey::Size);
        assert!(f2.desc, "方向与键原子随 Fire 下发(Task 19b)");
    }

    /// 隐藏项开关变化：同串重发、不改变排序；空会话不动。
    #[test]
    fn hidden_change_refires_same_query_without_touching_sort() {
        let base = Instant::now();
        let mut s = Session::new(Duration::from_millis(60));
        s.on_input("报告", base);
        let first = s
            .pending_at(base + Duration::from_millis(70))
            .expect("防抖到期应发起");
        assert_eq!(first.query, "报告");
        assert_eq!(first.sort, SortKey::Relevance);

        s.on_hidden_change(base + Duration::from_millis(200));
        let again = s
            .pending_at(base + Duration::from_millis(300))
            .expect("开关变化应重发");
        assert_eq!(again.query, "报告");
        assert_eq!(again.sort, SortKey::Relevance);
        assert_eq!(again.desc, first.desc);

        let mut empty = Session::new(Duration::from_millis(60));
        empty.on_hidden_change(base);
        assert!(empty
            .pending_at(base + Duration::from_millis(100))
            .is_none());
    }

    #[test]
    fn engine_lifecycle_states() {
        let mut s = Session::new(Duration::from_millis(10));
        s.mark_engine_starting();
        assert!(matches!(s.state, State::EngineStarting));
        s.mark_offline();
        assert!(matches!(s.state, State::Offline));
        s.mark_total(0);
        assert!(matches!(s.state, State::Empty), "total=0 → Empty");
        s.mark_total(3);
        assert!(
            matches!(s.state, State::Searching),
            "有结果回 Searching/有结果态"
        );
    }
}
