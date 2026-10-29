//! 结果行系统图标(visual.md §2.4「文件图标走系统真实图标」)。
//!
//! ## 缓存与去重
//!
//! - **类型图标**:UI 线程先解析 UTI(`platform::uti_for`,廉价的 UTType 查询),
//!   纹理、失败记录与在途请求都以 **UTI** 为键——`.jpg/.jpeg/.jpe` 等映射到同一
//!   UTI 的不同扩展名共享一份纹理与在途请求,不会重复解码 36MB TIFF。
//! - **APP bundle**(`.app` 目录):图标每个 APP 不同,按**路径**取
//!   (`NSWorkspace iconForFile:`,自然含用户自定义);缓存与在途去重同「自定义
//!   图标」通道(路径为键,失败落 `None` 回落 KEY_APP 类型图标 = 通用 APP 图标,
//!   不是文件夹图标——类型图标通道对 APP bundle 用固定 UTI
//!   `com.apple.application-bundle`)。
//! - **自定义图标**:用户给单个文件设过自定义图标时只能按路径取。路径检查结果
//!   (`Some` 纹理 / `None` = 无自定义或读取失败)跨帧保留;在途路径去重,不重复
//!   请求;`reset` 与 2048 条上限会清掉 `None`/最早条目。
//!   已知取舍:`Some` 纹理**不复查**源图标之后的移除/修改——一直保留到 2048 条
//!   上限淘汰或重启。可接受为性能取舍(每次查询不重复读资源属性)。
//!
//! ## 线程模型(为什么 AppKit 可在工作线程调用)
//!
//! 专用工作线程 `mce-icon-loader` 串行执行「资源读取 + `NSImage.TIFFRepresentation`
//! 数据化 + image 解码 + Lanczos 缩放」,只把 64×64 **预乘** RGBA 回传。UI 线程每帧只做:
//! drain 响应、限量大上传纹理、解析 UTI——零重活,不阻塞首屏/滚动。
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
/// 路径图标缓存上限(含自定义图标);超出按插入序淘汰最早。
const MAX_PATH_ICONS: usize = 2048;

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

/// 结果行图标缓存(跨帧保留;单位置一个)。
pub struct IconCache {
    /// 类型分组键 → UTI 标识符(同扩展名只解析一次 UTI)。
    key_to_uti: HashMap<String, String>,
    /// UTI 标识符 → 纹理(缓存键 = UTI,不是路径;visual.md §2.4)。
    type_icons: HashMap<String, egui::TextureHandle>,
    /// 取图失败的类型(UTI):不再重试,直接落兜底。
    failed_types: HashSet<String>,
    /// 路径 → 自定义图标;`None` = 无自定义或读取失败(均永久缓存到 reset/上限)。
    path_icons: HashMap<String, Option<egui::TextureHandle>>,
    /// 路径插入顺序(上限淘汰用;与 `path_icons` 同步维护)。
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
            failed_types: HashSet::new(),
            path_icons: HashMap::new(),
            path_order: VecDeque::new(),
            pending_types: HashSet::new(),
            pending_paths: HashSet::new(),
            pending_uploads: VecDeque::new(),
            req_tx: None,
            resp_rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            pending_since: None,
            #[cfg(test)]
            defer_requests: false,
        }
    }

    /// 每帧起点:drain worker 响应,限量上传纹理;有在途请求时轮询下一帧。
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        let drained = self.drain_responses();
        for _ in 0..UPLOAD_BUDGET {
            let Some(resp) = self.pending_uploads.pop_front() else {
                break;
            };
            self.apply_response(ctx, resp);
        }
        if !self.pending_uploads.is_empty() {
            ctx.request_repaint();
        }
        if self.pending_types.is_empty() && self.pending_paths.is_empty() {
            self.pending_since = None;
            return;
        }
        // 任何响应落地都重置超时计时;持续 5s 无响应则统一落失败,停止轮询。
        let now = Instant::now();
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
    pub fn icon(&mut self, ctx: &egui::Context, entry: &HitEntry) -> Option<egui::TextureId> {
        if is_app_bundle(&entry.name, entry.is_dir) {
            match self.path_icons.get(&entry.path) {
                Some(Some(tex)) => return Some(tex.id()),
                Some(None) => {} // 取图失败 → 通用 APP 类型图标兜底
                None => {
                    self.request_app(ctx, &entry.path);
                }
            }
            return self.type_icon(ctx, &entry.name, entry.is_dir);
        }
        match self.path_icons.get(&entry.path) {
            Some(Some(tex)) => return Some(tex.id()),
            Some(None) => {} // 已查无自定义图标 → 类型图标
            None => {
                self.request_path(ctx, &entry.path);
            }
        }
        self.type_icon(ctx, &entry.name, entry.is_dir)
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
    fn type_icon(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        is_dir: bool,
    ) -> Option<egui::TextureId> {
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

    /// 已上传纹理的类型图标命中。
    fn cached_type(&self, uti: &str) -> Option<egui::TextureId> {
        self.type_icons.get(uti).map(|tex| tex.id())
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

    /// 应用一条 worker 响应:上传纹理,写纹理/失败/无自定义缓存。
    fn apply_response(&mut self, ctx: &egui::Context, resp: IconResponse) {
        match resp {
            IconResponse::Type { uti, rgba } => {
                self.pending_types.remove(&uti);
                let name = format!("icon-type:{uti}");
                match rgba.and_then(|rgba| load_texture(ctx, &name, rgba)) {
                    Some(tex) => {
                        self.type_icons.insert(uti, tex);
                    }
                    None => {
                        self.failed_types.insert(uti);
                    }
                }
            }
            IconResponse::Path { path, rgba } => {
                self.pending_paths.remove(&path);
                let name = format!("icon-path:{path}");
                let tex = rgba.and_then(|rgba| load_texture(ctx, &name, rgba));
                self.store_path_icon(path, tex);
            }
            IconResponse::App { path, rgba } => {
                self.pending_paths.remove(&path);
                let name = format!("icon-app:{path}");
                let tex = rgba.and_then(|rgba| load_texture(ctx, &name, rgba));
                self.store_path_icon(path, tex);
            }
        }
    }

    /// 记录路径图标结果(含「无自定义/失败」),维护插入序并执行上限淘汰。
    fn store_path_icon(&mut self, path: String, icon: Option<egui::TextureHandle>) {
        if !self.path_icons.contains_key(&path) {
            self.path_order.push_back(path.clone());
        }
        self.path_icons.insert(path, icon);
        self.trim_path_icons();
    }

    /// 路径缓存超上限时按插入序淘汰最早条目。
    fn trim_path_icons(&mut self) {
        while self.path_icons.len() > MAX_PATH_ICONS {
            let victim = self.path_order.pop_front().or_else(|| {
                // 顺序表失配(理论不可达):退化为任意淘汰,仍保证硬上限。
                self.path_icons.keys().next().cloned()
            });
            let Some(victim) = victim else {
                break;
            };
            self.path_icons.remove(&victim);
        }
    }

    /// 每次唤起/复位时调用:清掉「无自定义/读取失败」的路径条目(它们只对上次
    /// 结果集有意义),自定义图标纹理保留;仍超上限则按插入序淘汰最早。
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

/// 平台层返回**预乘** RGBA(NSImage/ImageIO 的 TIFF 样本即预乘;resize 也在预乘上
/// 进行)。直接按预乘构造,避免 `from_rgba_unmultiplied` 二次乘 alpha 让半透明
/// 抗锯齿边缘发黑。
fn rgba_to_color_image(rgba: &[u8]) -> Option<egui::ColorImage> {
    if rgba.len() != ICON_PX as usize * ICON_PX as usize * 4 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_premultiplied(
        [ICON_PX as usize, ICON_PX as usize],
        rgba,
    ))
}

/// 上传 64×64 预乘 RGBA 到 egui 纹理(尺寸不符视为失败,不 panic)。
fn load_texture(ctx: &egui::Context, name: &str, rgba: Vec<u8>) -> Option<egui::TextureHandle> {
    let image = rgba_to_color_image(&rgba)?;
    Some(ctx.load_texture(name.to_owned(), image, egui::TextureOptions::LINEAR))
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

    fn texture(ctx: &egui::Context, name: &str) -> egui::TextureHandle {
        let image = egui::ColorImage::from_rgba_unmultiplied([ICON_PX as usize; 2], &rgba_fill(5));
        ctx.load_texture(name.to_owned(), image, egui::TextureOptions::LINEAR)
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
        let tex = texture(&ctx, "cached-type");
        let id = tex.id();
        cache
            .key_to_uti
            .insert("ext:jpg".to_owned(), "public.jpeg".to_owned());
        cache.type_icons.insert("public.jpeg".to_owned(), tex);

        assert_eq!(cache.icon(&ctx, &entry), Some(id), "纹理命中应返回 Some");
        assert_eq!(cache.cached_type("public.jpeg"), Some(id));
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
        let id = cache.icon(&ctx, &entry).expect("响应落地后应命中纹理");
        assert_eq!(cache.type_icons.len(), 1);
        assert_eq!(cache.cached_type("public.jpeg"), Some(id));
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
        let jpg_id = cache.icon(&ctx, &jpg).expect("jpg 命中共享纹理");
        let jpeg_id = cache.icon(&ctx, &jpeg).expect("jpeg 命中共享纹理");
        assert_eq!(jpg_id, jpeg_id);
        assert_eq!(cache.type_icons.len(), 1, "同 UTI 只存一份纹理");
    }

    #[test]
    fn premultiplied_rgba_is_not_multiplied_twice() {
        let mut rgba = vec![0u8; (ICON_PX * ICON_PX * 4) as usize];
        let px = [64u8, 32, 16, 128];
        rgba[..4].copy_from_slice(&px);
        let image = rgba_to_color_image(&rgba).expect("尺寸合法");
        let c = image.pixels[0];
        assert_eq!(
            [c.r(), c.g(), c.b(), c.a()],
            px,
            "预乘样本不得被二次乘 alpha(误用 unmultiplied 会变 [32,16,8,128])"
        );
        assert!(rgba_to_color_image(&rgba[..3]).is_none(), "尺寸不符应拒绝");
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
        let type_tex = texture(&ctx, "type-precedence");
        let type_id = type_tex.id();
        cache
            .type_icons
            .insert("public.plain-text".to_owned(), type_tex);

        assert_eq!(
            cache.icon(&ctx, &entry),
            Some(type_id),
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
        let custom_id = cache.path_icons[&entry.path]
            .as_ref()
            .expect("自定义图标已落地")
            .id();
        assert_ne!(custom_id, type_id);
        assert_eq!(
            cache.icon(&ctx, &entry),
            Some(custom_id),
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
        assert_eq!(cache.path_icons.len(), MAX_PATH_ICONS, "路径缓存上限 2048");
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
        let custom_id = cache.path_icons[custom].as_ref().unwrap().id();

        cache.on_reset();
        assert_eq!(
            cache.path_icons.len(),
            1,
            "reset 清掉「无自定义/读取失败」条目"
        );
        assert_eq!(cache.path_icons[custom].as_ref().unwrap().id(), custom_id);
        assert_eq!(cache.path_order.len(), 1);
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
