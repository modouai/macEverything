//! 结果行系统图标(visual.md §2.4「文件图标走系统真实图标」)。
//!
//! ## 纹理形态:单一图集(2026-11-01)
//!
//! 所有图标(类型 + 路径)都写进**一张** 64×64 单元格图集([`IconAtlas`]),
//! 绘制用 UV 定位。原因:实测每张独立图标纹理的驱动侧固定开销约 8.9MB
//! (64×64 位图仅 16KB),滚动停稳后 30 张可见图标即 ≈227MB;图集把纹理对象
//! 从「每图标一张」降为「全局一张」(≈8.9MB + 位图 1–4MB)。
//!
//! ## 缓存与去重
//!
//! - **类型图标**:UI 线程先解析 UTI(`platform::uti_for`,廉价的 UTType 查询),
//!   图集槽位、失败记录与在途请求都以 **UTI** 为键——`.jpg/.jpeg/.jpe` 等映射到同一
//!   UTI 的不同扩展名共享一份图块与在途请求,不会重复解码 36MB TIFF。
//! - **APP bundle**(`.app` 目录):图标每个 APP 不同,按**路径**取
//!   (`NSWorkspace iconForFile:`,自然含用户自定义);缓存与在途去重同「自定义
//!   图标」通道(路径为键,失败落 `None` 回落 KEY_APP 类型图标 = 通用 APP 图标,
//!   不是文件夹图标——类型图标通道对 APP bundle 用固定 UTI
//!   `com.apple.application-bundle`)。
//! - **自定义图标**:用户给单个文件设过自定义图标时只能按路径取。路径检查结果
//!   (`Some` 纹理 / `None` = 无自定义或读取失败)跨帧保留;在途路径去重,不重复
//!   请求;`reset` 与容量上限会清掉 `None`/最久未用条目。
//!   已知取舍:`Some` 纹理**不复查**源图标之后的移除/修改——一直保留到容量
//!   淘汰或重启。可接受为性能取舍(每次查询不重复读资源属性)。
//!
//! ## 线程模型(为什么 AppKit 可在工作线程调用)
//!
//! 专用工作线程 `mce-icon-loader` 串行执行「资源读取 + `NSImage.TIFFRepresentation`
//! 数据化 + image 解码 + Lanczos 缩放」,只把 64×64 **预乘** RGBA 回传。UI 线程每帧只做:
//! drain 响应、限量写入图集(每次 16KB partial 上传)、解析 UTI——零重活,不阻塞首屏/滚动。
//!
//! 依据:NSWorkspace 的图标查询可跨线程(Chromium `chrome/browser/icon_loader_mac.mm`
//! 即在独立线程调用 `iconForFile:`);worker 只对返回的 NSImage 做数据化,不创建/
//! 操作视图或窗口,也不触碰 egui。请求与响应在同一 worker 内串行,无并发 AppKit
//! 状态共享;每轮请求包在 autorelease pool 内,长驻线程不积压自动释放对象。
//!
//! 取图失败(`None`)由调用方画中性文档字形,不留空槽。类型纹理与类型失败记录
//! 跨帧保留;`PaletteState::reset` 清路径「无自定义/读取失败」条目,类型纹理与
//! 类型失败记录保留。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use protocol::HitEntry;

use crate::platform;
use crate::platform::ICON_PX;

/// 每帧最多上传的响应数(上传只建纹理,轻;防止一次涌出大量像素拷贝)。
const UPLOAD_BUDGET: usize = 8;
/// 工作线程在途时的响应轮询间隔(响应落地后下一帧即可见)。
const RESPONSE_POLL: Duration = Duration::from_millis(16);
/// 在途请求超时(持续无任何响应):防止 worker 卡死时永久 60fps 轮询。
const PENDING_TIMEOUT: Duration = Duration::from_secs(5);
/// 路径图标缓存上限(含自定义图标)。按**内存预算**定(2026-11-01 实测):
/// 图标纹理的驱动侧固定开销约 8.9MB/张(64×64 位图仅 16KB),条目数直接决定
/// 内存。32 条 ≈ 1.5 屏可见行(实测回退/小范围滚动不重载),超出按 LRU 淘汰。
/// 图集化后(见 [`IconAtlas`])条目本身不再各占一张纹理,此上限退回「条目数」
/// 语义,并防止无界增长。
const MAX_PATH_ICONS: usize = 32;
/// 图集每边槽数(8 列 × 8 行 = 64 槽起步;满时纵向倍增到 [`ATLAS_MAX_ROWS`])。
const ATLAS_COLS: usize = 8;
const ATLAS_INIT_ROWS: usize = 8;
/// 图集行数上限(32 行 = 256 槽 = 1024×2048 px;再满则淘汰最旧类型图标)。
const ATLAS_MAX_ROWS: usize = 32;
/// 滚动抑制的静默阈值:检测到滚动后再保持抑制 150ms,滚动停止且静默期满才
/// 恢复按需加载(给惯性滚动/连续滚轮留余量,避免「停一下就建一批纹理」)。
pub(crate) const SCROLL_SETTLE: Duration = Duration::from_millis(150);

/// 目录的类型分组键(与扩展名分组键空间隔离,避免文件扩展名撞键)。
const KEY_FOLDER: &str = "type:folder";
/// 无扩展名文件的类型分组键。
const KEY_NO_EXTENSION: &str = "type:noext";
/// APP bundle 的类型分组键(目录 + `.app` 后缀;兜底显示通用 APP 图标)。
const KEY_APP: &str = "type:app";
/// APP bundle 的系统 UTI(macOS 固定标识符,等同 `UTType.applicationBundle`),
/// 兜底类型图标的取图键。
const APP_UTI: &str = "com.apple.application-bundle";

/// 工作线程请求(UI 线程发送,单 worker 串行消费)。
#[derive(Debug)]
enum IconRequest {
    /// 按 UTI 取类型图标。
    Type(String),
    /// 按路径检查自定义图标。
    Path(String),
    /// 按路径取 APP bundle 自身图标(每个 APP 图标不同,无法按类型去重)。
    App(String),
}

/// 工作线程响应(UI 线程分帧上传纹理并落缓存)。
enum IconResponse {
    Type { uti: String, rgba: Option<Vec<u8>> },
    Path { path: String, rgba: Option<Vec<u8>> },
    App { path: String, rgba: Option<Vec<u8>> },
}

/// 一行图标的绘制句柄:图集纹理 + 该图标在**图集内**的 UV 子矩形。
///
/// 图集化动机(2026-11-01 实测):每张独立图标纹理的驱动侧固定开销约 8.9MB
/// (与 64×64 位图无关),滚动停稳后加载可见行即可堆出 30 张 ≈ 227MB。全部图标
/// 打进一张图集后,GL 纹理对象从「每图标一张」降为「全局一张」,内存从
/// N×8.9MB 降为 1×8.9MB + 图集位图(1–4MB)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconDraw {
    /// 图集纹理 id(全局唯一,三视口共用)。
    pub texture: egui::TextureId,
    /// 图集内的 UV 子矩形。
    pub uv: egui::Rect,
}

/// 图标图集:固定 64×64 单元格网格,slot 索引 → (列, 行)。
///
/// - 容量:8 列起步 8 行(64 槽),满时**只纵向倍增**到 [`ATLAS_MAX_ROWS`]
///   (slot 的行列映射不变,已有 UV 无需重算);
/// - 写入:响应落地时把 64×64 预乘 RGBA 拷进图集,并 `set_partial` 上传该单元格
///   (16KB/次,与逐图标纹理的整张上传相比可忽略);
/// - 槽位释放:Lru 淘汰/重置时归还 slot,复用不重建纹理。
struct IconAtlas {
    image: egui::ColorImage,
    handle: egui::TextureHandle,
    rows: usize,
    /// 可复用槽(淘汰归还)。
    free: Vec<usize>,
    /// 尚未分配过的下一个槽。
    next: usize,
}

impl IconAtlas {
    fn new(ctx: &egui::Context) -> Self {
        let px = ICON_PX as usize;
        let size = [px * ATLAS_COLS, px * ATLAS_INIT_ROWS];
        let image =
            egui::ColorImage::new(size, vec![egui::Color32::TRANSPARENT; size[0] * size[1]]);
        let handle = ctx.load_texture("icon-atlas", image.clone(), egui::TextureOptions::LINEAR);
        Self {
            image,
            handle,
            rows: ATLAS_INIT_ROWS,
            free: Vec::new(),
            next: 0,
        }
    }

    fn id(&self) -> egui::TextureId {
        self.handle.id()
    }

    fn capacity(&self) -> usize {
        ATLAS_COLS * self.rows
    }

    fn slot_xy(&self, slot: usize) -> (usize, usize) {
        let px = ICON_PX as usize;
        ((slot % ATLAS_COLS) * px, (slot / ATLAS_COLS) * px)
    }

    fn uv(&self, slot: usize) -> egui::Rect {
        let (x, y) = self.slot_xy(slot);
        let px = ICON_PX as f32;
        let (w, h) = (self.image.size[0] as f32, self.image.size[1] as f32);
        egui::Rect::from_min_max(
            egui::pos2(x as f32 / w, y as f32 / h),
            egui::pos2((x as f32 + px) / w, (y as f32 + px) / h),
        )
    }

    /// 分配一个槽:优先复用释放槽,其次未用槽,最后纵向扩容;到上限返回 None。
    fn alloc(&mut self, ctx: &egui::Context) -> Option<usize> {
        if let Some(slot) = self.free.pop() {
            return Some(slot);
        }
        if self.next < self.capacity() {
            let slot = self.next;
            self.next += 1;
            return Some(slot);
        }
        if !self.grow(ctx) {
            return None;
        }
        let slot = self.next;
        self.next += 1;
        Some(slot)
    }

    /// 纵向倍增(列数不变 → slot 的 (列, 行) 像素位置不变;归一化 UV 由绘制时
    /// 按当前图集尺寸实时计算,不缓存,故扩容后无需重算已交付的绘制句柄)。
    fn grow(&mut self, _ctx: &egui::Context) -> bool {
        if self.rows >= ATLAS_MAX_ROWS {
            return false;
        }
        let px = ICON_PX as usize;
        let new_rows = (self.rows * 2).min(ATLAS_MAX_ROWS);
        let size = [px * ATLAS_COLS, px * new_rows];
        let mut image =
            egui::ColorImage::new(size, vec![egui::Color32::TRANSPARENT; size[0] * size[1]]);
        let old_len = self.image.pixels.len();
        image.pixels[..old_len].copy_from_slice(&self.image.pixels);
        self.image = image;
        self.rows = new_rows;
        self.handle
            .set(self.image.clone(), egui::TextureOptions::LINEAR);
        log::debug!("图标图集扩容到 {:?}", self.image.size);
        true
    }

    /// 写入一个单元格并上传该 64×64 子区域(16KB partial delta)。
    ///
    /// 调用方保证 `rgba.len() == ICON_PX² × 4`(预乘)。
    fn write(&mut self, slot: usize, rgba: &[u8]) {
        let px = ICON_PX as usize;
        let (x, y) = self.slot_xy(slot);
        let w = self.image.size[0];
        let mut region = Vec::with_capacity(px * px);
        for row in 0..px {
            let src = &rgba[row * px * 4..(row + 1) * px * 4];
            for col in 0..px {
                let i = col * 4;
                let color = egui::Color32::from_rgba_premultiplied(
                    src[i],
                    src[i + 1],
                    src[i + 2],
                    src[i + 3],
                );
                self.image.pixels[(y + row) * w + x + col] = color;
                region.push(color);
            }
        }
        self.handle.set_partial(
            [x, y],
            egui::ColorImage::new([px, px], region),
            egui::TextureOptions::LINEAR,
        );
    }
}

/// 结果行图标缓存(跨帧保留;单位置一个)。
pub struct IconCache {
    /// 类型分组键 → UTI 标识符(同扩展名只解析一次 UTI)。
    key_to_uti: HashMap<String, String>,
    /// UTI 标识符 → 图集槽位(缓存键 = UTI,不是路径;visual.md §2.4)。
    type_icons: HashMap<String, usize>,
    /// 类型图标的插入序(图集槽耗尽时淘汰最旧者).
    type_order: VecDeque<String>,
    /// 取图失败的类型(UTI):不再重试,直接落兜底。
    failed_types: HashSet<String>,
    /// 路径 → 自定义图标槽位;`None` = 无自定义或读取失败(均保留到 reset/上限)。
    path_icons: HashMap<String, Option<usize>>,
    /// 图标图集(懒创建:首个图标落地时建;三视口共用一张纹理)。
    atlas: Option<IconAtlas>,
    /// 路径 LRU 序(头 = 最久未用,尾 = 最近使用;命中/覆盖即移尾;
    /// 上限淘汰从头取。与 `path_icons` 同步维护)。
    path_order: VecDeque<String>,
    /// 在途类型请求(UTI);同一 UTI 不重复发请求。
    pending_types: HashSet<String>,
    /// 在途自定义图标检查(路径);同一路径不重复发请求。
    pending_paths: HashSet<String>,
    /// 已回传、等待 UI 线程上传纹理的响应(分帧限量消费)。
    pending_uploads: VecDeque<IconResponse>,
    req_tx: Option<Sender<IconRequest>>,
    resp_rx: Option<Receiver<IconResponse>>,
    /// worker 取消标志(Drop 置位并丢弃 sender;不 join,避免阻塞 UI/退出)。
    cancel: Arc<AtomicBool>,
    /// 在途请求计时起点;任何响应落地后重置,超时则统一落失败。
    pending_since: Option<Instant>,
    /// 滚动抑制截止时刻(见 [`SCROLL_SETTLE`]):此刻之前视为「列表正在滚动」——
    /// 不发起新加载、不上传新纹理,只交付已缓存图标,其余行画占位。
    /// 动机(A6b 归因):滚动经过的每一行都取图标 → 每个新图标建一张 GL 纹理,
    /// LRU 淘汰的旧纹理被 GL 兼容层延迟释放,实测 311 块 × 8.9MB ≈ 2.7GB 滞留。
    scroll_settle_until: Option<Instant>,
    /// 测试构造:无 worker;请求只记在途标记,由测试注入响应。
    #[cfg(test)]
    defer_requests: bool,
}

impl IconCache {
    /// 生产构造:启动专用图标加载线程(取图/解码/缩放全在 worker)。
    pub fn new() -> Self {
        let mut cache = Self::empty();
        match spawn_worker(Arc::clone(&cache.cancel)) {
            Ok((tx, rx)) => {
                cache.req_tx = Some(tx);
                cache.resp_rx = Some(rx);
            }
            Err(err) => log::warn!("图标加载线程启动失败,结果行将回落兜底字形:{err}"),
        }
        cache
    }

    /// 无工作线程构造(单测:只测缓存/去重,不发起 AppKit 请求)。
    #[cfg(test)]
    pub(crate) fn without_worker() -> Self {
        let mut cache = Self::empty();
        cache.defer_requests = true;
        cache
    }

    /// 可注入假 mpsc 的测试构造:发请求进 `req_rx`,测试用 `resp_tx` 注入响应。
    #[cfg(test)]
    fn with_test_channels() -> (Self, Receiver<IconRequest>, Sender<IconResponse>) {
        let (req_tx, req_rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::channel();
        let mut cache = Self::empty();
        cache.req_tx = Some(req_tx);
        cache.resp_rx = Some(resp_rx);
        (cache, req_rx, resp_tx)
    }

    fn empty() -> Self {
        Self {
            key_to_uti: HashMap::new(),
            type_icons: HashMap::new(),
            type_order: VecDeque::new(),
            failed_types: HashSet::new(),
            path_icons: HashMap::new(),
            atlas: None,
            path_order: VecDeque::new(),
            pending_types: HashSet::new(),
            pending_paths: HashSet::new(),
            pending_uploads: VecDeque::new(),
            req_tx: None,
            resp_rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            pending_since: None,
            scroll_settle_until: None,
            #[cfg(test)]
            defer_requests: false,
        }
    }

    /// 每帧起点:drain worker 响应,限量上传纹理;有在途请求时轮询下一帧。
    ///
    /// 滚动期([`Self::note_scroll`] 后的静默窗)**不上传**:上传即建 GL 纹理,
    /// 滚动经过的大量行会让驱动侧滞留大块纹理内存(A6b 归因);响应留在队列,
    /// 滚动停止后一次性限量补齐(队列长度受在途请求上限约束,不会膨胀)。
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.begin_frame_at(ctx, Instant::now());
    }

    /// [`Self::begin_frame`] 的显式时钟版本(单测注入时间)。
    pub(crate) fn begin_frame_at(&mut self, ctx: &egui::Context, now: Instant) {
        let drained = self.drain_responses();
        let scrolling = self.scrolling_at(now);
        if !scrolling {
            for _ in 0..UPLOAD_BUDGET {
                let Some(resp) = self.pending_uploads.pop_front() else {
                    break;
                };
                self.apply_response(ctx, resp);
            }
        }
        if scrolling || !self.pending_uploads.is_empty() {
            // 滚动中:排队响应到期后再排一帧(静默窗结束即恢复上传)。
            ctx.request_repaint_after(if scrolling {
                SCROLL_SETTLE
            } else {
                RESPONSE_POLL
            });
        }
        if self.pending_types.is_empty() && self.pending_paths.is_empty() {
            self.pending_since = None;
            return;
        }
        // 任何响应落地都重置超时计时;持续 5s 无响应则统一落失败,停止轮询。
        let since = if drained {
            now
        } else {
            self.pending_since.unwrap_or(now)
        };
        self.pending_since = Some(since);
        if now.duration_since(since) >= PENDING_TIMEOUT {
            log::warn!(
                "图标加载 {:?} 无响应:在途请求统一落失败/无自定义",
                PENDING_TIMEOUT
            );
            self.pending_since = None;
            let types: Vec<String> = self.pending_types.drain().collect();
            for uti in types {
                self.failed_types.insert(uti);
            }
            let paths: Vec<String> = self.pending_paths.drain().collect();
            for path in paths {
                self.store_path_icon(path, None);
            }
        } else {
            // worker 不回打 egui:仅在等待期轮询,请求全部落地即停。
            ctx.request_repaint_after(RESPONSE_POLL);
        }
    }

    /// 取一行的图标纹理:APP bundle 按路径取自身图标;其余条目自定义图标优先;
    /// 未命中/无自定义/失败回落类型图标。
    ///
    /// APP bundle(`.app` 目录)的图标每个 APP 不同,`NSWorkspace iconForFile:`
    /// 才能给到(自然含用户自定义);未命中/失败时本帧先用类型图标(KEY_APP →
    /// 通用 APP 图标,不是文件夹图标)。
    pub fn icon(&mut self, ctx: &egui::Context, entry: &HitEntry) -> Option<IconDraw> {
        self.icon_at(ctx, entry, Instant::now())
    }

    /// 滚动状态采样:列表滚动中每帧调用(偏移变化帧 + 惯性/滚轮期间)。
    ///
    /// 语义:此后 [`SCROLL_SETTLE`] 内的调用只交付缓存图标;到期后自动恢复
    /// 按需加载——调用方无需显式「恢复」。
    pub fn note_scroll(&mut self, ctx: &egui::Context) {
        self.scroll_settle_until = Some(Instant::now() + SCROLL_SETTLE);
        // 静默窗到期后必须有一帧来恢复加载(否则滚动停止后不再有帧)。
        ctx.request_repaint_after(SCROLL_SETTLE);
    }

    /// 当前是否处于滚动抑制窗(测试直接注入时间)。
    fn scrolling_at(&self, now: Instant) -> bool {
        self.scroll_settle_until.is_some_and(|until| now < until)
    }

    /// [`Self::icon`] 的显式时钟版本(单测注入时间,不依赖真实时钟)。
    pub(crate) fn icon_at(
        &mut self,
        ctx: &egui::Context,
        entry: &HitEntry,
        now: Instant,
    ) -> Option<IconDraw> {
        if self.scrolling_at(now) {
            return self.cached_icon(entry);
        }
        if is_app_bundle(&entry.name, entry.is_dir) {
            if self.path_icons.contains_key(&entry.path) {
                self.touch(&entry.path);
                if let Some(Some(slot)) = self.path_icons.get(&entry.path) {
                    return self.draw_slot(*slot);
                }
                // 取图失败 → 通用 APP 类型图标兜底
            } else {
                self.request_app(ctx, &entry.path);
            }
            return self.type_icon(ctx, &entry.name, entry.is_dir);
        }
        if self.path_icons.contains_key(&entry.path) {
            self.touch(&entry.path);
            if let Some(Some(slot)) = self.path_icons.get(&entry.path) {
                return self.draw_slot(*slot);
            }
            // 已查无自定义图标 → 类型图标
        } else {
            self.request_path(ctx, &entry.path);
        }
        self.type_icon(ctx, &entry.name, entry.is_dir)
    }

    /// 槽位 → 绘制句柄(图集必已存在:槽位只由图集写入产生)。
    fn draw_slot(&self, slot: usize) -> Option<IconDraw> {
        let atlas = self.atlas.as_ref()?;
        Some(IconDraw {
            texture: atlas.id(),
            uv: atlas.uv(slot),
        })
    }

    /// 测试观测面:在途请求数(路径, 类型)。用于验证调用点确实报告了滚动。
    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> (usize, usize) {
        (self.pending_paths.len(), self.pending_types.len())
    }

    /// 只读缓存命中(滚动抑制窗内唯一允许的路径):不发起请求、不做 UTType 解析。
    ///
    /// 命中规则与 [`Self::icon`] 一致,但所有「未命中→请求」的分支都退化为
    /// `None`(调用方画占位);类型图标只查**已解析过**的键。
    fn cached_icon(&mut self, entry: &HitEntry) -> Option<IconDraw> {
        if is_app_bundle(&entry.name, entry.is_dir) {
            return self
                .path_icons
                .get(&entry.path)
                .and_then(|slot| slot.as_ref().and_then(|s| self.draw_slot(*s)));
        }
        if let Some(Some(slot)) = self.path_icons.get(&entry.path) {
            let slot = *slot;
            return self.draw_slot(slot);
        }
        // 类型图标:键已解析过才查(滚动期不做 UTType 查询)。
        let key = type_key(&entry.name, entry.is_dir);
        let uti = self.key_to_uti.get(&key)?;
        let uti = uti.clone();
        self.cached_type(&uti)
    }

    /// LRU 记账:命中的路径移到队尾(最近使用)。
    ///
    /// O(容量)线性定位——容量 128,每行图标每帧一次,量级无虞。
    fn touch(&mut self, path: &str) {
        if let Some(pos) = self.path_order.iter().position(|p| p == path) {
            let path = self.path_order.remove(pos).expect("position 有效");
            self.path_order.push_back(path);
        }
    }

    /// APP bundle 图标请求入队(按路径,与自定义图标检查共用去重/缓存空间);
    /// 返回是否新入队。
    fn request_app(&mut self, ctx: &egui::Context, path: &str) -> bool {
        if self.path_icons.contains_key(path) || !self.pending_paths.insert(path.to_owned()) {
            return false;
        }
        if !self.send(IconRequest::App(path.to_owned())) {
            // 通道不可用(worker 已退出):按「无图」落缓存,回落类型图标。
            self.pending_paths.remove(path);
            self.store_path_icon(path.to_owned(), None);
        } else {
            ctx.request_repaint_after(RESPONSE_POLL);
        }
        true
    }

    /// 路径请求入队(去重);返回是否新入队。
    fn request_path(&mut self, ctx: &egui::Context, path: &str) -> bool {
        if self.path_icons.contains_key(path) || !self.pending_paths.insert(path.to_owned()) {
            return false;
        }
        if !self.send(IconRequest::Path(path.to_owned())) {
            // 通道不可用(worker 已退出):按「无自定义图标」落缓存,不反复重试。
            self.pending_paths.remove(path);
            self.store_path_icon(path.to_owned(), None);
        } else {
            ctx.request_repaint_after(RESPONSE_POLL);
        }
        true
    }

    /// 类型图标:先解析 UTI,再以 UTI 查纹理/失败/在途;未命中才发请求。
    fn type_icon(&mut self, ctx: &egui::Context, name: &str, is_dir: bool) -> Option<IconDraw> {
        let key = type_key(name, is_dir);
        let uti = match self.key_to_uti.get(&key) {
            Some(uti) => uti.clone(),
            None => {
                // APP bundle 兜底用固定系统 UTI(通用 APP 图标);其余廉价解析
                // (UTType 查询)留在 UI 线程;取图/解码在 worker。
                let uti = if key == KEY_APP {
                    APP_UTI.to_owned()
                } else {
                    platform::uti_for(extension(name), is_dir)
                };
                self.key_to_uti.insert(key, uti.clone());
                uti
            }
        };
        if let Some(id) = self.cached_type(&uti) {
            return Some(id);
        }
        if self.failed_types.contains(&uti) || self.pending_types.contains(&uti) {
            return None;
        }
        self.pending_types.insert(uti.clone());
        if self.send(IconRequest::Type(uti.clone())) {
            ctx.request_repaint_after(RESPONSE_POLL);
        } else {
            // 通道不可用:记失败,避免每帧重试。
            self.pending_types.remove(&uti);
            self.failed_types.insert(uti);
        }
        None
    }

    /// 已入图集的类型图标命中。
    fn cached_type(&self, uti: &str) -> Option<IconDraw> {
        self.type_icons
            .get(uti)
            .and_then(|slot| self.draw_slot(*slot))
    }

    /// 发送请求。测试构造(无 worker)只保留在途标记,返回成功由测试注入响应。
    fn send(&self, req: IconRequest) -> bool {
        #[cfg(test)]
        if self.defer_requests {
            return true;
        }
        match &self.req_tx {
            Some(tx) => tx.send(req).is_ok(),
            None => false,
        }
    }

    /// 非阻塞收集 worker 响应;返回是否收到至少一条。
    /// worker 断开则留痕,并把在途请求统一落失败/无自定义。
    fn drain_responses(&mut self) -> bool {
        let mut drained = false;
        let mut disconnected = false;
        if let Some(rx) = &self.resp_rx {
            loop {
                match rx.try_recv() {
                    Ok(resp) => {
                        self.pending_uploads.push_back(resp);
                        drained = true;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            log::warn!("图标加载线程已断开:在途请求统一落失败/无自定义");
            // 一次性收口:清掉收发两端,后续 begin_frame 不再重复走 Disconnected/warn。
            self.req_tx = None;
            self.resp_rx = None;
            let types: Vec<String> = self.pending_types.drain().collect();
            for uti in types {
                self.failed_types.insert(uti);
            }
            let paths: Vec<String> = self.pending_paths.drain().collect();
            for path in paths {
                self.store_path_icon(path, None);
            }
        }
        drained
    }

    /// 应用一条 worker 响应:把像素写进图集(partial 上传),落缓存。
    fn apply_response(&mut self, ctx: &egui::Context, resp: IconResponse) {
        match resp {
            IconResponse::Type { uti, rgba } => {
                self.pending_types.remove(&uti);
                match rgba.and_then(|rgba| self.place_icon(ctx, &rgba)) {
                    Some(slot) => {
                        self.type_icons.insert(uti.clone(), slot);
                        self.type_order.push_back(uti);
                    }
                    None => {
                        self.failed_types.insert(uti);
                    }
                }
            }
            IconResponse::Path { path, rgba } => {
                self.pending_paths.remove(&path);
                let slot = rgba.and_then(|rgba| self.place_icon(ctx, &rgba));
                self.store_path_icon(path, slot);
            }
            IconResponse::App { path, rgba } => {
                self.pending_paths.remove(&path);
                let slot = rgba.and_then(|rgba| self.place_icon(ctx, &rgba));
                self.store_path_icon(path, slot);
            }
        }
    }

    /// 把一份 64×64 预乘 RGBA 放进图集,返回槽位;尺寸不符落败(不 panic)。
    ///
    /// 槽位分配顺序:释放槽 → 未用槽 → 纵向扩容;图集到上限(256 槽)时淘汰
    /// 最旧类型图标腾槽。类型图标是共享资源(同 UTI 全列表复用),淘汰代价低。
    fn place_icon(&mut self, ctx: &egui::Context, rgba: &[u8]) -> Option<usize> {
        if rgba.len() != ICON_PX as usize * ICON_PX as usize * 4 {
            return None;
        }
        if self.atlas.is_none() {
            self.atlas = Some(IconAtlas::new(ctx));
        }
        loop {
            let slot = self.atlas.as_mut().expect("图集已建").alloc(ctx);
            if let Some(slot) = slot {
                self.atlas.as_mut().expect("图集已建").write(slot, rgba);
                return Some(slot);
            }
            // 图集满:淘汰最旧类型图标腾槽(无类型可淘汰则放弃)。
            let victim = self.type_order.pop_front()?;
            if let Some(old) = self.type_icons.remove(&victim) {
                self.atlas.as_mut().expect("图集已建").free.push(old);
            }
        }
    }

    /// 记录路径图标结果(含「无自定义/失败」),维护 LRU 序并执行上限淘汰。
    fn store_path_icon(&mut self, path: String, slot: Option<usize>) {
        if let Some(prev) = self.path_icons.insert(path.clone(), slot) {
            if prev != slot {
                // 覆盖已有条目(理论不可达:在途去重挡住重复请求):归还旧槽。
                if let (Some(old), Some(atlas)) = (prev, self.atlas.as_mut()) {
                    atlas.free.push(old);
                }
            }
            self.touch(&path);
        } else {
            self.path_order.push_back(path);
        }
        self.trim_path_icons();
    }

    /// 路径缓存超上限时按 LRU 淘汰最久未用条目(归还图集槽位复用)。
    fn trim_path_icons(&mut self) {
        while self.path_icons.len() > MAX_PATH_ICONS {
            let victim = self.path_order.pop_front().or_else(|| {
                // 顺序表失配(理论不可达):退化为任意淘汰,仍保证硬上限。
                self.path_icons.keys().next().cloned()
            });
            let Some(victim) = victim else {
                break;
            };
            if let Some(Some(slot)) = self.path_icons.remove(&victim) {
                if let Some(atlas) = self.atlas.as_mut() {
                    atlas.free.push(slot);
                }
            }
        }
    }

    /// 每次唤起/复位时调用:清掉「无自定义/读取失败」的路径条目(它们只对上次
    /// 结果集有意义),自定义图标纹理保留;仍超上限则按 LRU 淘汰最久未用。
    pub fn on_reset(&mut self) {
        self.path_icons.retain(|_, icon| icon.is_some());
        let existing: HashSet<String> = self.path_icons.keys().cloned().collect();
        self.path_order.retain(|path| existing.contains(path));
        self.trim_path_icons();
    }
}

impl Drop for IconCache {
    fn drop(&mut self) {
        // 取消标志 + 丢弃 sender:worker 若空闲会立即退出;若卡在慢 AppKit 调用,
        // 本轮结束后看到标志/断连即退出。不 join——UI/退出不被 worker 阻塞。
        self.cancel.store(true, Ordering::Relaxed);
        self.req_tx = None;
    }
}

/// 取图失败时的中性文档字形(visual.md §2.4 兜底,不留空槽)。
///
/// 按传入矩形等比缩放:悬浮条 32×32 与完整窗口 16×16 共用同一形状。
pub(crate) fn draw_fallback(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let s = rect.width() / 32.0;
    let c = rect.center();
    let (half_w, half_h, fold) = (7.5 * s, 10.0 * s, 5.0 * s);
    let (left, right) = (c.x - half_w, c.x + half_w);
    let (top, bottom) = (c.y - half_h, c.y + half_h);
    let stroke = egui::Stroke::new((1.4 * s).max(0.8), color);
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(left, top),
            egui::pos2(right - fold, top),
            egui::pos2(right, top + fold),
            egui::pos2(right, bottom),
            egui::pos2(left, bottom),
        ],
        stroke,
    ));
    // 右上折角。
    painter.line_segment(
        [
            egui::pos2(right - fold, top),
            egui::pos2(right - fold, top + fold),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(right - fold, top + fold),
            egui::pos2(right, top + fold),
        ],
        stroke,
    );
    // 两条内容线,强化「文档」字形。
    let line = egui::Stroke::new((1.0 * s).max(0.6), color);
    painter.line_segment(
        [
            egui::pos2(left + 3.0 * s, c.y + 1.0 * s),
            egui::pos2(right - 3.0 * s, c.y + 1.0 * s),
        ],
        line,
    );
    painter.line_segment(
        [
            egui::pos2(left + 3.0 * s, c.y + 4.5 * s),
            egui::pos2(right - 5.0 * s, c.y + 4.5 * s),
        ],
        line,
    );
}

/// 启动串行取图线程;线程只做 AppKit 取图 + 像素转换,不触碰视图/egui。
fn spawn_worker(
    cancel: Arc<AtomicBool>,
) -> std::io::Result<(Sender<IconRequest>, Receiver<IconResponse>)> {
    let (req_tx, req_rx) = mpsc::channel();
    let (resp_tx, resp_rx) = mpsc::channel();
    thread::Builder::new()
        .name("mce-icon-loader".to_owned())
        .spawn(move || worker_loop(req_rx, resp_tx, cancel))?;
    Ok((req_tx, resp_rx))
}

/// worker 主循环:请求/响应严格串行;每轮包 autorelease pool。
fn worker_loop(
    req_rx: Receiver<IconRequest>,
    resp_tx: Sender<IconResponse>,
    cancel: Arc<AtomicBool>,
) {
    while !cancel.load(Ordering::Relaxed) {
        let Ok(req) = req_rx.recv() else {
            break; // UI 侧已销毁
        };
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let resp = match req {
            IconRequest::Type(uti) => {
                let rgba = objc2::rc::autoreleasepool(|_| platform::type_icon_rgba(&uti));
                IconResponse::Type { uti, rgba }
            }
            IconRequest::Path(path) => {
                let rgba = objc2::rc::autoreleasepool(|_| platform::custom_icon_rgba(&path));
                IconResponse::Path { path, rgba }
            }
            IconRequest::App(path) => {
                let rgba = objc2::rc::autoreleasepool(|_| platform::app_icon_rgba(&path));
                IconResponse::App { path, rgba }
            }
        };
        if resp_tx.send(resp).is_err() {
            break;
        }
    }
}

/// 文件名扩展名(与 `type_key` 同一口径,平台层按它解析 UTI)。
///
/// 语义(单测钉住):取最后一个 `.` 之后、非空的一段;隐藏文件(`.gitignore`)
/// 与尾点(`report.`)不算扩展名;`.config.json` 的扩展名是 `json`。
fn extension(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

/// 类型分组键(UTI 解析前的廉价去重键;缓存纹理本身按 UTI 存)。
///
/// APP bundle 与目录、无扩展名各有专用键;扩展名小写归并(`PDF` 与 `pdf` 同类型)。
fn type_key(name: &str, is_dir: bool) -> String {
    if is_dir {
        return if is_app_bundle(name, is_dir) {
            KEY_APP.to_owned()
        } else {
            KEY_FOLDER.to_owned()
        };
    }
    match extension(name) {
        Some(ext) => format!("ext:{}", ext.to_ascii_lowercase()),
        None => KEY_NO_EXTENSION.to_owned(),
    }
}

/// APP bundle 判定:目录且 display 名以 `.app` 结尾(ASCII 大小写不敏感)。
/// 与 `engine::query::is_app_entry` 同口径(engine 侧钉排序,本侧钉图标)。
fn is_app_bundle(name: &str, is_dir: bool) -> bool {
    const SUFFIX: &[u8] = b".app";
    is_dir
        && name.len() >= SUFFIX.len()
        && name.as_bytes()[name.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str) -> HitEntry {
        HitEntry {
            name: name.to_owned(),
            path: format!("/tmp/mce-icon-test/{name}"),
            size: None,
            mtime: None,
            is_dir: false,
        }
    }

    fn rgba_fill(v: u8) -> Vec<u8> {
        let mut px = vec![0u8; (ICON_PX * ICON_PX * 4) as usize];
        for chunk in px.as_chunks_mut::<4>().0 {
            *chunk = [v, v, v, 255];
        }
        px
    }

    /// 测试:把一份纯色图块放进图集,返回其绘制句柄。
    fn put_icon(cache: &mut IconCache, ctx: &egui::Context, v: u8) -> IconDraw {
        let slot = cache.place_icon(ctx, &rgba_fill(v)).expect("图集槽位");
        cache.draw_slot(slot).expect("图集已存在")
    }

    /// 测试:造一个已缓存的类型图标并返回句柄。
    fn put_type(cache: &mut IconCache, ctx: &egui::Context, uti: &str, v: u8) -> IconDraw {
        let rgba = rgba_fill(v);
        let slot = cache.place_icon(ctx, &rgba).expect("图集槽位");
        cache.type_icons.insert(uti.to_owned(), slot);
        cache.type_order.push_back(uti.to_owned());
        cache.cached_type(uti).expect("类型图标已入图集")
    }

    /// 测试:造一个已缓存的路径图标并返回句柄。
    fn put_path(cache: &mut IconCache, ctx: &egui::Context, path: &str, v: u8) -> IconDraw {
        let rgba = rgba_fill(v);
        let slot = cache.place_icon(ctx, &rgba).expect("图集槽位");
        cache.store_path_icon(path.to_owned(), Some(slot));
        cache.draw_slot(slot).expect("图集已存在")
    }

    /// 测试隔离:路径已检查且无自定义图标,避免类型测试顺带挂路径请求。
    fn mark_path_checked(cache: &mut IconCache, entry: &HitEntry) {
        cache.path_icons.insert(entry.path.clone(), None);
    }

    #[test]
    fn type_key_folders_use_dedicated_key() {
        assert_eq!(type_key("Photos.2026", true), KEY_FOLDER);
        assert_eq!(type_key("README", true), KEY_FOLDER);
        assert_ne!(
            type_key("Photos.2026", true),
            type_key("Photos.2026", false)
        );
    }

    /// APP 判定 = 目录 + `.app` 后缀(ASCII 大小写不敏感);非目录与普通目录不算。
    #[test]
    fn app_bundle_detection() {
        assert!(is_app_bundle("Safari.app", true));
        assert!(is_app_bundle("Tool.APP", true));
        assert!(!is_app_bundle("fake.app", false), "非目录不判 APP");
        assert!(!is_app_bundle("plain", true), "无后缀目录不判 APP");
        assert!(
            !is_app_bundle("notes.app.txt", true),
            "非 .app 结尾不判 APP"
        );
    }

    /// APP bundle 的类型分组键走专用 KEY_APP:兜底显示通用 APP 图标而非文件夹图标。
    #[test]
    fn type_key_app_bundles_use_dedicated_key() {
        assert_eq!(type_key("Safari.app", true), KEY_APP);
        assert_eq!(type_key("Tool.APP", true), KEY_APP);
        assert_ne!(type_key("plain", true), KEY_APP);
    }

    /// APP bundle 走按路径取图(NSWorkspace iconForFile:,每个 APP 图标不同),
    /// 不走类型图标通道。
    #[test]
    fn app_bundle_requests_icon_by_path() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, _resp_tx) = IconCache::with_test_channels();
        let mut entry = hit("Safari.app");
        entry.is_dir = true;
        let _ = cache.icon(&ctx, &entry);
        assert!(
            matches!(req_rx.try_recv(), Ok(IconRequest::App(p)) if p == entry.path),
            "APP bundle 应发按路径取图请求"
        );
    }

    /// APP 图标按路径取图失败:落「无图」缓存(不重复请求),回落专用类型图标
    /// (KEY_APP → 通用 APP 图标),而非文件夹图标。
    #[test]
    fn app_bundle_failure_falls_back_to_app_type_icon() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, resp_tx) = IconCache::with_test_channels();
        let mut entry = hit("Broken.app");
        entry.is_dir = true;
        let _ = cache.icon(&ctx, &entry);
        resp_tx
            .send(IconResponse::App {
                path: entry.path.clone(),
                rgba: None,
            })
            .unwrap();
        cache.begin_frame(&ctx);
        assert!(
            matches!(cache.path_icons.get(&entry.path), Some(None)),
            "取图失败应落缓存"
        );

        // 失败后不再重复发路径请求;兜底解析出通用 APP 类型图标请求。
        let _ = cache.icon(&ctx, &entry);
        // 第一次 icon() 已发出的 App 请求先消费掉,再断言本轮只发兜底 Type 请求。
        assert!(
            matches!(req_rx.try_recv(), Ok(IconRequest::App(p)) if p == entry.path),
            "首条应为第一次调用的 App 请求"
        );
        match req_rx.try_recv() {
            Ok(IconRequest::Type(uti)) => {
                assert_eq!(uti, APP_UTI, "兜底应为通用 APP 类型图标而非文件夹图标")
            }
            other => panic!("失败后应只发兜底 Type 请求,实际 {other:?}"),
        }
        loop {
            match req_rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Ok(IconRequest::App(p)) => panic!("失败路径不得重复请求:{p}"),
                Ok(_) => {}
            }
        }
    }

    #[test]
    fn type_key_uses_last_extension_lowercased() {
        assert_eq!(type_key("REPORT.PDF", false), "ext:pdf");
        assert_eq!(type_key("archive.tar.gz", false), "ext:gz");
        assert_eq!(type_key("notes.Markdown", false), "ext:markdown");
    }

    #[test]
    fn type_key_hidden_and_no_extension() {
        assert_eq!(type_key("README", false), KEY_NO_EXTENSION);
        assert_eq!(type_key(".gitignore", false), KEY_NO_EXTENSION);
        assert_eq!(type_key("report.", false), KEY_NO_EXTENSION);
        // 隐藏文件带扩展名:扩展名照常生效。
        assert_eq!(type_key(".config.json", false), "ext:json");
    }

    #[test]
    fn extension_semantics_match_type_key() {
        assert_eq!(extension("archive.tar.gz"), Some("gz"));
        assert_eq!(extension(".config.json"), Some("json"));
        assert_eq!(extension(".gitignore"), None);
        assert_eq!(extension("report."), None);
        assert_eq!(extension("README"), None);
        assert_eq!(extension("a.b.c"), Some("c"));
    }

    #[test]
    fn cached_type_hit_returns_texture() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let entry = hit("cached.jpg");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:jpg".to_owned(), "public.jpeg".to_owned());
        let draw = put_type(&mut cache, &ctx, "public.jpeg", 5);

        assert_eq!(
            cache.icon(&ctx, &entry).map(|d| (d.texture, d.uv)),
            Some((draw.texture, draw.uv)),
            "图集命中应返回 Some"
        );
        assert_eq!(
            cache.cached_type("public.jpeg").map(|d| (d.texture, d.uv)),
            Some((draw.texture, draw.uv))
        );
        assert!(cache.pending_types.is_empty());
    }

    #[test]
    fn failed_type_returns_none_without_new_request() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let entry = hit("broken.pdf");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:pdf".to_owned(), "com.adobe.pdf".to_owned());
        cache.failed_types.insert("com.adobe.pdf".to_owned());

        assert_eq!(cache.icon(&ctx, &entry), None);
        assert!(cache.pending_types.is_empty(), "失败类型不得重新发起请求");
    }

    #[test]
    fn known_no_custom_icon_does_not_requeue_path() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let entry = hit("plain.txt");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:txt".to_owned(), "public.plain-text".to_owned());
        cache.failed_types.insert("public.plain-text".to_owned());

        assert_eq!(cache.icon(&ctx, &entry), None);
        assert!(
            cache.pending_paths.is_empty(),
            "已查无自定义图标的路径不得重复入队"
        );
    }

    #[test]
    fn in_flight_paths_are_deduped() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let path = "/tmp/mce-icon-test/a.txt";

        assert!(cache.request_path(&ctx, path), "首次应入队");
        assert!(!cache.request_path(&ctx, path), "在途重复入队应被拒");
        assert_eq!(cache.pending_paths.len(), 1);

        cache.apply_response(
            &ctx,
            IconResponse::Path {
                path: path.to_owned(),
                rgba: None,
            },
        );
        assert!(matches!(cache.path_icons.get(path), Some(None)));
        assert!(!cache.request_path(&ctx, path), "已检查路径不得重复请求");
    }

    #[test]
    fn in_flight_type_is_deduped_and_late_response_hits() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let entry = hit("late.jpg");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:jpg".to_owned(), "public.jpeg".to_owned());

        assert_eq!(cache.icon(&ctx, &entry), None, "请求当帧落兜底");
        assert_eq!(cache.pending_types.len(), 1);
        assert_eq!(cache.icon(&ctx, &entry), None, "在途重复调用仍落兜底");
        assert_eq!(cache.pending_types.len(), 1, "同一 UTI 不得重复请求");

        cache.apply_response(
            &ctx,
            IconResponse::Type {
                uti: "public.jpeg".to_owned(),
                rgba: Some(rgba_fill(9)),
            },
        );
        assert!(cache.pending_types.is_empty());
        let draw = cache.icon(&ctx, &entry).expect("响应落地后应命中图集");
        assert_eq!(cache.type_icons.len(), 1);
        assert_eq!(
            cache.cached_type("public.jpeg").map(|d| (d.texture, d.uv)),
            Some((draw.texture, draw.uv))
        );
    }

    #[test]
    fn aliases_share_one_request_and_texture() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let jpg = hit("alias.jpg");
        let jpeg = hit("alias.jpeg");
        mark_path_checked(&mut cache, &jpg);
        mark_path_checked(&mut cache, &jpeg);
        // 平台层把两种扩展名解析到同一 UTI;这里直接钉住该映射。
        let uti = "public.jpeg";
        cache
            .key_to_uti
            .insert("ext:jpg".to_owned(), uti.to_owned());
        cache
            .key_to_uti
            .insert("ext:jpeg".to_owned(), uti.to_owned());

        assert_eq!(cache.icon(&ctx, &jpg), None);
        assert_eq!(cache.icon(&ctx, &jpeg), None);
        assert_eq!(cache.pending_types.len(), 1, "同 UTI 只允许一个在途请求");
        assert!(cache.pending_types.contains(uti));

        cache.apply_response(
            &ctx,
            IconResponse::Type {
                uti: uti.to_owned(),
                rgba: Some(rgba_fill(11)),
            },
        );
        let jpg_draw = cache.icon(&ctx, &jpg).expect("jpg 命中共享图集槽");
        let jpeg_draw = cache.icon(&ctx, &jpeg).expect("jpeg 命中共享图集槽");
        assert_eq!(
            (jpg_draw.texture, jpg_draw.uv),
            (jpeg_draw.texture, jpeg_draw.uv)
        );
        assert_eq!(cache.type_icons.len(), 1, "同 UTI 只存一份图集槽");
    }

    #[test]
    fn premultiplied_rgba_is_not_multiplied_twice() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let mut rgba = vec![0u8; (ICON_PX * ICON_PX * 4) as usize];
        let px = [64u8, 32, 16, 128];
        rgba[..4].copy_from_slice(&px);
        let slot = cache.place_icon(&ctx, &rgba).expect("尺寸合法");
        let atlas = cache.atlas.as_ref().expect("图集已建");
        let (x, y) = atlas.slot_xy(slot);
        let c = atlas.image.pixels[y * atlas.image.size[0] + x];
        assert_eq!(
            [c.r(), c.g(), c.b(), c.a()],
            px,
            "预乘样本不得被二次乘 alpha(误用 unmultiplied 会变 [32,16,8,128])"
        );
        assert!(
            cache.place_icon(&ctx, &rgba[..3]).is_none(),
            "尺寸不符应拒绝"
        );
    }

    #[test]
    fn begin_frame_uploads_at_most_eight_and_requests_repaint() {
        let ctx = egui::Context::default();
        let (mut cache, _req_rx, resp_tx) = IconCache::with_test_channels();
        for i in 0..(UPLOAD_BUDGET + 2) {
            resp_tx
                .send(IconResponse::Path {
                    path: format!("/tmp/mce-icon-test/upload-{i}"),
                    rgba: None,
                })
                .unwrap();
        }

        cache.begin_frame(&ctx);
        assert_eq!(
            cache.path_icons.len(),
            UPLOAD_BUDGET,
            "每帧最多上传 {UPLOAD_BUDGET} 条响应"
        );
        assert_eq!(cache.pending_uploads.len(), 2, "剩余响应留到下帧");
        assert!(ctx.has_requested_repaint(), "剩余响应应排下一帧");

        cache.begin_frame(&ctx);
        assert_eq!(cache.path_icons.len(), UPLOAD_BUDGET + 2);
        assert!(cache.pending_uploads.is_empty());
    }

    #[test]
    fn worker_disconnect_fails_pending_requests() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, resp_tx) = IconCache::with_test_channels();
        let entry = hit("gone.pdf");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:pdf".to_owned(), "com.adobe.pdf".to_owned());

        assert_eq!(cache.icon(&ctx, &entry), None);
        assert_eq!(cache.pending_types.len(), 1);
        assert!(
            matches!(req_rx.try_recv(), Ok(IconRequest::Type(uti)) if uti == "com.adobe.pdf"),
            "类型请求应进入 worker 通道"
        );

        drop(resp_tx); // 模拟 worker 退出
        cache.begin_frame(&ctx);
        assert!(cache.pending_types.is_empty(), "断连后在途请求应清空");
        assert!(
            cache.failed_types.contains("com.adobe.pdf"),
            "在途类型落失败"
        );
        assert!(cache.pending_since.is_none(), "无在途请求时不保留计时");
        assert!(
            cache.resp_rx.is_none(),
            "断连处理后应一次性清掉接收端(防每帧重复 warn)"
        );

        // 二次 begin_frame:断连处理必须幂等,不再重复落失败/挂上传。
        let failed = cache.failed_types.len();
        let paths = cache.path_icons.len();
        let uploads = cache.pending_uploads.len();
        cache.begin_frame(&ctx);
        assert!(cache.resp_rx.is_none(), "二次 begin_frame 不得复活接收端");
        assert!(cache.pending_types.is_empty(), "在途集合不得复活");
        assert!(cache.pending_paths.is_empty(), "在途路径不得复活");
        assert_eq!(cache.failed_types.len(), failed, "不得重复落失败");
        assert_eq!(cache.path_icons.len(), paths, "不得重复处理路径");
        assert_eq!(cache.pending_uploads.len(), uploads, "不得重复入上传队列");
    }

    #[test]
    fn path_custom_icon_takes_precedence_over_type_icon() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, resp_tx) = IconCache::with_test_channels();
        let entry = hit("custom.txt");
        cache
            .key_to_uti
            .insert("ext:txt".to_owned(), "public.plain-text".to_owned());
        let type_draw = put_type(&mut cache, &ctx, "public.plain-text", 7);

        assert_eq!(
            cache.icon(&ctx, &entry).map(|d| (d.texture, d.uv)),
            Some((type_draw.texture, type_draw.uv)),
            "自定义图标在途时先用类型图标"
        );
        assert_eq!(cache.pending_paths.len(), 1);
        assert!(
            matches!(req_rx.try_recv(), Ok(IconRequest::Path(path)) if path == entry.path),
            "路径检查请求应进入 worker 通道"
        );

        resp_tx
            .send(IconResponse::Path {
                path: entry.path.clone(),
                rgba: Some(rgba_fill(3)),
            })
            .unwrap();
        cache.begin_frame(&ctx);
        let custom = cache
            .draw_slot(
                *cache.path_icons[&entry.path]
                    .as_ref()
                    .expect("自定义图标已落地"),
            )
            .expect("图集已存在");
        assert_ne!(custom.uv, type_draw.uv, "自定义与类型图标应是不同槽位");
        assert_eq!(
            cache.icon(&ctx, &entry).map(|d| (d.texture, d.uv)),
            Some((custom.texture, custom.uv)),
            "自定义图标优先于类型图标"
        );
    }

    #[test]
    fn pending_requests_time_out_and_mark_failed() {
        let ctx = egui::Context::default();
        let (mut cache, _req_rx, _resp_tx) = IconCache::with_test_channels();
        let entry = hit("stuck.pdf");
        mark_path_checked(&mut cache, &entry);
        cache
            .key_to_uti
            .insert("ext:pdf".to_owned(), "com.adobe.pdf".to_owned());

        assert_eq!(cache.icon(&ctx, &entry), None);
        assert_eq!(cache.pending_types.len(), 1);

        // 模拟在途请求已等待超过 5s 且无任何响应。
        cache.pending_since = Some(Instant::now() - PENDING_TIMEOUT);
        cache.begin_frame(&ctx);
        assert!(cache.pending_types.is_empty(), "超时应清空在途请求");
        assert!(
            cache.failed_types.contains("com.adobe.pdf"),
            "超时类型落失败"
        );
        assert!(cache.pending_since.is_none(), "超时后停止轮询计时");
    }

    #[test]
    fn on_reset_clears_no_custom_and_caps_path_icons() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        for i in 0..(MAX_PATH_ICONS + 2) {
            cache.apply_response(
                &ctx,
                IconResponse::Path {
                    path: format!("/tmp/mce-icon-test/cap-{i}"),
                    rgba: None,
                },
            );
        }
        assert_eq!(cache.path_icons.len(), MAX_PATH_ICONS, "路径缓存上限 32");
        assert!(!cache.path_icons.contains_key("/tmp/mce-icon-test/cap-0"));
        assert!(!cache.path_icons.contains_key("/tmp/mce-icon-test/cap-1"));

        let custom = "/tmp/mce-icon-test/cap-custom";
        cache.apply_response(
            &ctx,
            IconResponse::Path {
                path: custom.to_owned(),
                rgba: Some(rgba_fill(4)),
            },
        );
        assert!(cache.path_icons[custom].is_some(), "自定义图标应保留");
        let custom_slot = *cache.path_icons[custom].as_ref().unwrap();

        cache.on_reset();
        assert_eq!(
            cache.path_icons.len(),
            1,
            "reset 清掉「无自定义/读取失败」条目"
        );
        assert_eq!(cache.path_icons[custom].as_ref().unwrap(), &custom_slot);
        assert_eq!(cache.path_order.len(), 1);
    }

    /// 滚动抑制:滚动窗内**不得**发起新请求(路径/类型都不发)——A6b 归因:
    /// 滚动经过的每一行都取图标 → 每图标一张 GL 纹理 → 2.7GB 滞留。
    /// 突变(去掉 scrolling 守卫)时本测必红:会发出一条 Path 请求。
    #[test]
    fn scrolling_suppresses_new_icon_requests() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, _resp_tx) = IconCache::with_test_channels();
        let t = Instant::now();
        cache.scroll_settle_until = Some(t + SCROLL_SETTLE);
        let entry = hit("scrolling.txt");

        assert_eq!(cache.icon_at(&ctx, &entry, t), None, "滚动期应落占位");
        assert!(req_rx.try_recv().is_err(), "滚动期不得发起任何加载请求");
        assert!(cache.pending_paths.is_empty(), "滚动期不得记在途路径");
        assert!(cache.pending_types.is_empty(), "滚动期不得记在途类型");
    }

    /// 滚动期仍交付已缓存图标(缓存命中零成本,不新建纹理/请求)。
    #[test]
    fn scrolling_still_serves_cached_icons() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let t = Instant::now();
        cache.scroll_settle_until = Some(t + SCROLL_SETTLE);

        // ① 类型图标已缓存(键已解析):滚动期照常返回。
        let entry = hit("cached.pdf");
        cache
            .key_to_uti
            .insert("ext:pdf".to_owned(), "com.adobe.pdf".to_owned());
        let type_draw = put_type(&mut cache, &ctx, "com.adobe.pdf", 6);
        cache.path_icons.insert(entry.path.clone(), None);
        assert_eq!(
            cache.icon_at(&ctx, &entry, t).map(|d| (d.texture, d.uv)),
            Some((type_draw.texture, type_draw.uv))
        );

        // ② 路径自定义图标已缓存:滚动期照常返回。
        let custom = hit("custom.txt");
        let custom_draw = put_path(&mut cache, &ctx, &custom.path, 8);
        assert_eq!(
            cache.icon_at(&ctx, &custom, t).map(|d| (d.texture, d.uv)),
            Some((custom_draw.texture, custom_draw.uv))
        );

        // ③ 未解析的类型键:滚动期不做 UTType 解析、不发请求。
        let fresh = hit("fresh.xyz");
        assert_eq!(cache.icon_at(&ctx, &fresh, t), None);
        assert!(
            !cache.key_to_uti.contains_key("ext:xyz"),
            "滚动期不得解析 UTI"
        );
    }

    /// 静默期满(150ms)自动恢复按需加载——调用方无需显式恢复。
    #[test]
    fn requests_resume_after_scroll_settle() {
        let ctx = egui::Context::default();
        let (mut cache, req_rx, _resp_tx) = IconCache::with_test_channels();
        let t = Instant::now();
        cache.scroll_settle_until = Some(t + SCROLL_SETTLE);

        let entry = hit("resume.txt");
        assert_eq!(cache.icon_at(&ctx, &entry, t), None);
        assert!(req_rx.try_recv().is_err(), "窗内不请求");
        assert_eq!(
            cache.icon_at(&ctx, &entry, t + SCROLL_SETTLE),
            None,
            "到期帧仍落占位(本帧发请求,纹理下帧才有)"
        );
        assert!(
            matches!(req_rx.try_recv(), Ok(IconRequest::Path(p)) if p == entry.path),
            "静默期满应恢复发起路径检查请求"
        );
    }

    /// 滚动期不上传纹理(响应留在队列),静默期满后一次性补齐——保证滚动期间
    /// 零新建 GL 纹理,而不仅仅是零新请求。
    #[test]
    fn scrolling_defers_texture_uploads_until_settle() {
        let ctx = egui::Context::default();
        let (mut cache, _req_rx, resp_tx) = IconCache::with_test_channels();
        let t = Instant::now();
        resp_tx
            .send(IconResponse::Path {
                path: "/tmp/mce-icon-test/deferred".to_owned(),
                rgba: None,
            })
            .unwrap();
        cache.scroll_settle_until = Some(t + SCROLL_SETTLE);

        cache.begin_frame_at(&ctx, t);
        assert_eq!(cache.pending_uploads.len(), 1, "滚动期响应不得上传");
        assert!(cache.path_icons.is_empty(), "滚动期不得落纹理/缓存");

        cache.begin_frame_at(&ctx, t + SCROLL_SETTLE);
        assert!(cache.pending_uploads.is_empty(), "静默期满应补齐上传");
        assert!(
            cache.path_icons.contains_key("/tmp/mce-icon-test/deferred"),
            "静默期满后响应应落缓存"
        );
    }

    /// 图集核心收益:无论多少图标(类型 + 路径),全部落在**同一张**纹理上,
    /// UV 各不相同。突变(退回每图标一张纹理)时 texture id 会互不相同,本测红。
    #[test]
    fn atlas_packs_all_icons_into_one_texture() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let a = put_type(&mut cache, &ctx, "public.jpeg", 1);
        let b = put_type(&mut cache, &ctx, "public.png", 2);
        let c = put_path(&mut cache, &ctx, "/tmp/mce-icon-test/x.app", 3);
        let d = put_icon(&mut cache, &ctx, 4);
        assert_eq!(a.texture, b.texture, "不同图标必须共用同一张图集纹理");
        assert_eq!(a.texture, c.texture);
        assert_eq!(a.texture, d.texture);
        let uvs = [a.uv, b.uv, c.uv, d.uv];
        for i in 0..uvs.len() {
            for j in (i + 1)..uvs.len() {
                assert_ne!(uvs[i], uvs[j], "不同图标必须占不同 UV 单元格");
            }
            assert!(
                uvs[i].width() > 0.0 && uvs[i].height() > 0.0,
                "UV 必须是有效子矩形"
            );
        }
    }

    /// 纵向扩容:超过初始 64 槽后图集行数翻倍,**既有槽位的 UV 保持不变**
    /// (列数不变 → slot 的行列映射不变),纹理 id 也不变。
    #[test]
    fn atlas_grows_vertically_without_moving_existing_uvs() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let first = put_icon(&mut cache, &ctx, 1);
        let mut last = first;
        for v in 0..(ATLAS_COLS * ATLAS_INIT_ROWS + 4) {
            last = put_icon(&mut cache, &ctx, (v % 250) as u8);
        }
        let atlas = cache.atlas.as_ref().expect("图集已建");
        assert!(
            atlas.rows > ATLAS_INIT_ROWS,
            "超过初始槽位后应纵向扩容,实际 rows={}",
            atlas.rows
        );
        assert_eq!(first.texture, last.texture, "扩容后仍是同一张纹理");
        // 归一化 UV 随高度变化(÷h),但换算回像素仍是同一格;绘制时实时计算。
        let uv = cache.draw_slot(0).unwrap().uv;
        let (w, h) = (atlas.image.size[0] as f32, atlas.image.size[1] as f32);
        let px = ICON_PX as f32;
        assert!((uv.min.x * w).abs() < 0.01 && (uv.min.y * h).abs() < 0.01);
        assert!((uv.width() * w - px).abs() < 0.01);
        assert!((uv.height() * h - px).abs() < 0.01);
    }

    /// 淘汰归还槽位复用:路径缓存超上限淘汰后,新图标应复用释放槽,
    /// 不无限扩容(图集槽位数受缓存上限约束)。
    #[test]
    fn atlas_reuses_slots_freed_by_lru_eviction() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        for i in 0..(MAX_PATH_ICONS + 1) {
            let path = format!("/tmp/mce-icon-test/reuse-{i}");
            let slot = cache.place_icon(&ctx, &rgba_fill(1)).unwrap();
            cache.store_path_icon(path, Some(slot));
        }
        let atlas = cache.atlas.as_ref().unwrap();
        assert!(
            atlas.rows == ATLAS_INIT_ROWS,
            "淘汰释放的槽位应被复用,不应扩容(实际 rows={})",
            atlas.rows
        );
        assert!(atlas.free.is_empty() || atlas.next <= MAX_PATH_ICONS + 1);
    }

    /// 突变钉:容量常量必须是 32(2026-11-01 按内存预算定:每张独立图标纹理
    /// 驱动侧固定开销 ~8.9MB;图集化后条目不再各占一张纹理,上限只防无界增长;
    /// 若有人调回大容量,此测先红,提示去规格处重新定线)。
    #[test]
    fn path_icon_capacity_is_pinned_to_spec() {
        assert_eq!(MAX_PATH_ICONS, 32);
    }

    /// LRU 语义:命中把条目移到队尾;淘汰从头取最久未用——最旧但刚被命中的
    /// 条目必须存活,淘汰的是「没人再看」的第二旧条目。突变(命中不 touch,
    /// 退回 FIFO)时 lru-0 被淘汰、lru-1 存活,本测双断言皆红。
    #[test]
    fn path_icon_cache_evicts_least_recently_used() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let path = |i: usize| format!("/tmp/mce-icon-test/lru-{i}");
        for i in 0..MAX_PATH_ICONS {
            cache.apply_response(
                &ctx,
                IconResponse::Path {
                    path: path(i),
                    rgba: None,
                },
            );
        }
        assert_eq!(cache.path_icons.len(), MAX_PATH_ICONS);

        // 命中最旧的 lru-0(icon() 走缓存命中分支并 touch)。
        let mut entry = hit("lru-0");
        entry.path = path(0);
        let _ = cache.icon(&ctx, &entry);
        // LRU 序:头应是 lru-1(lru-0 已被移到队尾)。
        assert_eq!(
            cache.path_order.front().map(String::as_str),
            Some(path(1).as_str()),
            "命中后最久未用应是 lru-1"
        );

        // 再进 2 条:挤掉 lru-1、lru-2,lru-0(最旧但刚用过)必须存活。
        for i in (MAX_PATH_ICONS)..(MAX_PATH_ICONS + 2) {
            cache.apply_response(
                &ctx,
                IconResponse::Path {
                    path: path(i),
                    rgba: None,
                },
            );
        }
        assert_eq!(cache.path_icons.len(), MAX_PATH_ICONS, "硬上限不变");
        assert!(cache.path_icons.contains_key(&path(0)), "刚命中的条目存活");
        assert!(
            !cache.path_icons.contains_key(&path(1)),
            "最久未用的 lru-1 被淘汰"
        );
        assert!(
            !cache.path_icons.contains_key(&path(2)),
            "次旧的 lru-2 被淘汰"
        );
        assert!(
            cache.path_icons.contains_key(&path(3)),
            "更晚插入且未命中的 lru-3 存活"
        );
    }

    /// 命中「无自定义/读取失败」条目同样刷新 LRU(它们也是缓存条目,
    /// 不刷新会在长会话里被误淘汰,导致同路径反复发 AppKit 检查)。
    #[test]
    fn touching_failed_path_entries_refreshes_recency() {
        let ctx = egui::Context::default();
        let mut cache = IconCache::without_worker();
        let path = |i: usize| format!("/tmp/mce-icon-test/fail-{i}");
        for i in 0..MAX_PATH_ICONS {
            cache.apply_response(
                &ctx,
                IconResponse::Path {
                    path: path(i),
                    rgba: None,
                },
            );
        }
        let mut entry = hit("fail-0");
        entry.path = path(0);
        let _ = cache.icon(&ctx, &entry);

        // 再进 1 条:挤掉新的最久未用 lru-1;刚被命中(虽是「无自定义」)的
        // lru-0 必须存活——否则下帧同路径又要发一次 AppKit 检查。
        cache.apply_response(
            &ctx,
            IconResponse::Path {
                path: path(MAX_PATH_ICONS),
                rgba: None,
            },
        );
        assert_eq!(cache.path_icons.len(), MAX_PATH_ICONS);
        assert!(
            cache.path_icons.contains_key(&path(0)),
            "命中的失败条目按 LRU 存活"
        );
        assert!(
            !cache.path_icons.contains_key(&path(1)),
            "淘汰的是未命中的 lru-1"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn jpeg_aliases_resolve_to_same_uti() {
        // 仅做 UTType 查询(不取图),验证别名去重的前提。
        let jpg = platform::uti_for(Some("jpg"), false);
        let jpeg = platform::uti_for(Some("jpeg"), false);
        assert_eq!(jpg, "public.jpeg");
        assert_eq!(jpeg, jpg, "jpg/jpeg 应映射同一 UTI");
    }
}
