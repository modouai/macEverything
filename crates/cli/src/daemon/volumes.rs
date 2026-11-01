//! 外接磁盘卷监听(external-volumes spec,daemon 子模块)。
//!
//! 每 [`POLL_INTERVAL`] 轮询 `/Volumes`,与「期望挂载集」对账(tech.md §3):
//! 新本地卷 → 加 root + 后台全量扫 + 按卷 FSEvents 流;卷消失/停用 →
//! 前缀清除 + 停流。选轮询而非 DiskArbitration:零新增 unsafe FFI、
//! 3s 延迟可接受,与 v1-core「事件只当信号,对账才是真相」哲学一致。
//!
//! 事件水位纪律(关键不变量):**卷流不参与全局 seen/applied 水位**。多流共享
//! 水位时,卷消费方的 `apply_batch` 会在主消费方未应用时抬高已应用层,重启重放
//! 将漏掉主事件(永不自愈)。卷事件不记水位 → 重启后由主流(last_event_id)对卷
//! 目录重放 diff,幂等补齐——只多付 diff,不丢事件。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use engine::config::Config;
use engine::fsevents::{self};

use super::{
    consume_events, ensure_mutable, flush, mutable_base_slot, DaemonPaths, EngineState,
    IndexHolder, LockFailFast, LockRecover, RwLockFailFast, SCAN_CHUNK,
};

/// 摘除子树后同步作废 Overlay 账本(anchors/overlaid/dir_ids 锚定被摘 base 条目号,
/// 不清账则查询侧把陈旧子树误判为有效)。仅在 Overlaid 态需要;_holder 借用在
/// 调用方持锁期间传入。
fn purge_overlay_after_prefix_removal(holder: &mut IndexHolder, prefix: &Path) {
    if let IndexHolder::Overlaid(st) = holder {
        let view = st.base.as_view();
        st.overlay.purge_prefix(view, prefix);
    }
}

/// 卷轮询间隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// 网络文件系统类型黑名单(与 MNT_LOCAL 双保险;v1 不做网络卷)。
const NETWORK_FSTYPES: [&[u8]; 6] = [b"smbfs", b"nfs", b"afp", b"cifs", b"webdav", b"ftp"];

/// `/Volumes` 根。
const VOLUMES_ROOT: &str = "/Volumes";

/// 枚举当前挂载的**本地**外接卷(非 dot 目录、MNT_LOCAL、非网络文件系统)。
pub fn discover_local_volumes() -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(VOLUMES_ROOT) else {
        return vec![];
    };
    let mut vols: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            match p.file_name().and_then(|n| n.to_str()) {
                // dot 目录:Time Machine 快照(/Volumes/.timemachine/…)等,不索引
                Some(n) if n.starts_with('.') => return false,
                None => return false,
                _ => {}
            }
            is_local_volume(p)
        })
        .collect();
    vols.sort();
    vols
}

/// 路径是否本地卷:`statfs` 的 `MNT_LOCAL` 置位且文件系统非网络类型。
fn is_local_volume(p: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY:statfs 只读路径字符串;输出结构由调用方栈上提供并全量初始化。
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return false;
    }
    if (st.f_flags as i32) & libc::MNT_LOCAL == 0 {
        return false;
    }
    // 系统卷剔除(nobrowse 标志):Recovery/Preboot/VM 等系统卷带 MNT_DONTBROWSE。
    if (st.f_flags as i32) & libc::MNT_DONTBROWSE != 0 {
        return false;
    }
    // 自挂载校验:statfs 的挂载点必须就是该 /Volumes 路径本身。
    // /Volumes/Macintosh HD 是 Firmlink 数据卷入口(真实挂载点 /System/Volumes/Data),
    // statfs 穿透后挂载点并非自身——那是系统盘,不是「插入的外接盘」。
    let mnt_len = st
        .f_mntonname
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(st.f_mntonname.len());
    let mnton: Vec<u8> = st.f_mntonname[..mnt_len].iter().map(|&c| c as u8).collect();
    if mnton.as_slice() != p.as_os_str().as_encoded_bytes() {
        return false;
    }
    // f_fstypename: [c_char; 16],NUL 截断的文件系统名。
    let len = st
        .f_fstypename
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(st.f_fstypename.len());
    let fstype: Vec<u8> = st.f_fstypename[..len].iter().map(|&c| c as u8).collect();
    !NETWORK_FSTYPES.contains(&fstype.as_slice())
}

/// 期望挂载集:配置放行 + 本地性判定后的卷根列表。
pub fn desired_roots(cfg: &Config) -> Vec<PathBuf> {
    if !cfg.auto_index_volumes {
        return vec![];
    }
    discover_local_volumes()
        .into_iter()
        .filter(|p| match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => !cfg.volumes_excluded.iter().any(|x| x == n),
            None => false,
        })
        .collect()
}

/// 卷路径 `/Volumes/<名>`;非卷内路径返回 None。
fn volume_root_of(p: &Path) -> Option<PathBuf> {
    let mut comps = p.components();
    if comps.next()?.as_os_str() != "/" {
        return None;
    }
    if comps.next()?.as_os_str() != "Volumes" {
        return None;
    }
    let name = comps.next()?;
    Some(Path::new(VOLUMES_ROOT).join(name.as_os_str()))
}

/// 卷监听主循环(cold_start 末尾拉起;进程退出随进程结束)。
pub fn monitor_loop(state: Arc<EngineState>, paths: DaemonPaths) {
    loop {
        if state.stopping.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
        if let Err(e) = reconcile(&state, &paths) {
            log::warn!("卷对账失败: {e:#}");
        }
    }
}

/// 一轮对账:收敛「当前索引卷集」到「期望挂载集」。先卸后挂。
fn reconcile(state: &Arc<EngineState>, paths: &DaemonPaths) -> anyhow::Result<()> {
    let cfg = state.cfg.lock_recover().clone();
    let desired: HashSet<PathBuf> = desired_roots(&cfg).into_iter().collect();
    let current: HashSet<PathBuf> = state
        .roots
        .lock_recover()
        .iter()
        .filter(|r| volume_root_of(r).is_some())
        .cloned()
        .collect();
    for vol in current.difference(&desired) {
        unmount_volume(state, paths, vol);
    }
    for vol in desired.difference(&current) {
        mount_volume(state, paths, vol);
    }
    Ok(())
}

/// 挂载:roots/索引种子记账 → 建卷流(SINCE_NOW,早于扫描,窗口变更幂等补齐) →
/// 全量扫(批间检查卷仍在,消失即中止转清除)→ flush。本函数在监听线程内
/// 顺序执行:同一时刻至多一个卷在扫,不与主索引争写锁带宽。
fn mount_volume(state: &Arc<EngineState>, paths: &DaemonPaths, vol: &Path) {
    log::info!("卷挂载,开始索引: {}", vol.display());
    // 冷启动直挂 Shared 后的卷写入在此按需物化(条目号 1:1,dir_map 免重建)
    ensure_mutable(state);
    state.roots.lock_recover().push(vol.to_path_buf());
    {
        let mut h = state.holder.write_fail_fast("holder");
        let Some(idx) = mutable_base_slot(&mut h) else {
            log::warn!("卷索引推迟(主索引非可变态,如冷启动/整理中),下轮重试");
            state.roots.lock_recover().retain(|r| r != vol);
            return;
        };
        let mut map = state.dir_map.lock_fail_fast("dir_map");
        engine::scanner::seed_roots(idx, &mut map, std::slice::from_ref(&vol.to_path_buf()));
    }
    // 建卷流:先于扫描。SINCE_NOW 之后的变化由流事件补,扫描覆盖当下,重叠 diff 幂等。
    let vol_str = vol.to_string_lossy().into_owned();
    match fsevents::start_stream(
        &[vol_str.as_str()],
        fsevents::SINCE_NOW,
        0.1,
        fsevents::FILE_EVENTS,
    ) {
        Ok((stream, rx)) => {
            state
                .volume_streams
                .lock_recover()
                .insert(vol.to_path_buf(), stream);
            let st = state.clone();
            let p = paths.clone();
            // 卷流消费:不写水位(见模块注释),退出不触发降级(有意 stop,非故障)。
            super::spawn_guarded("volume-consume", move || {
                consume_events(st, p, None, rx, false, false, true);
            });
        }
        Err(e) => log::warn!("卷流建立失败({e}),该卷仅静态索引无实时监听: {vol_str}"),
    }

    scan_volume_into(state, paths, vol);
}

/// 卷根种子 + 全量扫入索引(批间检查卷仍在)→ flush。
///
/// **前缀清除前置**:上次会话/上次挂载可能残留该卷条目,直接 seed+apply 会与
/// 残留重复(每文件双条目);先清后扫,确定性收敛。运行期挂载与启动补扫共用。
pub fn scan_volume_into(state: &Arc<EngineState>, paths: &DaemonPaths, vol: &Path) {
    // 本轮扫描的世代快照:任一卸载(含本轮自己的中止路径)推进世代即失效,
    // 批间校验不过直接放弃,不与新一轮扫描交错应用。
    let my_epoch = state.volume_scan_epoch.load(Ordering::Relaxed);
    // 直挂 Shared 态同样在此按需物化(运行期补扫路径不经 mount_volume 时)
    ensure_mutable(state);
    {
        let mut h = state.holder.write_fail_fast("holder");
        let mut map = state.dir_map.lock_fail_fast("dir_map");
        let Some(idx) = mutable_base_slot(&mut h) else {
            log::warn!("卷扫描推迟(主索引非可变态),下轮重试: {}", vol.display());
            return;
        };
        engine::scanner::remove_prefix(idx, &mut map, vol);
        // 前缀清除先行:Overlay 账本同步作废(残留 anchor 会让陈旧子树在合成视图复活)。
        // 与 seed 分两段取槽:purge 需 holder 全借用,与 idx 槽借用不交叠。
        purge_overlay_after_prefix_removal(&mut h, vol);
        let Some(idx) = mutable_base_slot(&mut h) else {
            unreachable!("purge 不改 holder 形态,槽位必仍在")
        };
        engine::scanner::seed_roots(idx, &mut map, std::slice::from_ref(&vol.to_path_buf()));
    }
    let cfg = state.cfg.lock_recover().clone();
    let excludes = engine::reconcile::expand_excludes(&cfg.excluded);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let (recs, table, gaps) =
        engine::scanner::collect_records(&[vol.to_path_buf()], &excludes, threads);
    let mut applied = 0usize;
    for chunk in recs.chunks(SCAN_CHUNK) {
        // 批间检查:扫描中途拔盘 → 中止,清除由下一轮对账兜底;
        // 世代漂移(拔出又重插,新一轮扫描已接管)同样放弃本轮残余批次。
        if !vol.exists() {
            log::warn!("卷扫描中消失,中止: {}", vol.display());
            unmount_volume(state, paths, vol);
            return;
        }
        if state.volume_scan_epoch.load(Ordering::Relaxed) != my_epoch {
            log::info!("卷扫描世代漂移,放弃本轮残余批次: {}", vol.display());
            return;
        }
        let mut h = state.holder.write_fail_fast("holder");
        if let Some(idx) = mutable_base_slot(&mut h) {
            let mut map = state.dir_map.lock_fail_fast("dir_map");
            engine::scanner::apply_records(idx, &mut map, chunk, &table);
            applied += chunk.len();
            state.dirty.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
    }
    state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
    flush(state, paths);
    log::info!(
        "卷索引完成: {} 条目 {applied} 条(io_errors {})",
        vol.display(),
        gaps.io_errors
    );
}

/// 卸载/停用:停卷流 → 前缀清除 → roots 移除 → flush。幂等。
fn unmount_volume(state: &Arc<EngineState>, paths: &DaemonPaths, vol: &Path) {
    log::info!("卷卸载,清除索引: {}", vol.display());
    // 世代推进:使该卷任何在途扫描批次失效——「扫盘中拔出又立刻重插」时,
    // 旧线程的 vol.exists() 检查对重插后的卷恒真,无世代则残余批次会在
    // 新扫之后继续应用,产生重复/陈旧条目(自审 2026-10-21)。
    state.volume_scan_epoch.fetch_add(1, Ordering::Relaxed);
    if let Some(mut stream) = state.volume_streams.lock_recover().remove(vol) {
        stream.stop(); // Drop 内 stop;卷消费线程因 rx 断开退出,volume_owned 不触发降级
    }
    // 直挂 Shared 态在此按需物化,前缀清除照常生效(否则只能靠重试路径)
    ensure_mutable(state);
    let removed = {
        let mut h = state.holder.write_fail_fast("holder");
        let Some(idx) = mutable_base_slot(&mut h) else {
            log::warn!("卷清除推迟(主索引非可变态),下轮重试");
            return;
        };
        let mut map = state.dir_map.lock_fail_fast("dir_map");
        let removed = engine::scanner::remove_prefix(idx, &mut map, vol);
        purge_overlay_after_prefix_removal(&mut h, vol);
        removed
    };
    state.roots.lock_recover().retain(|r| r != vol);
    flush(state, paths);
    log::info!("卷清除完成: {} 条目 {removed} 条", vol.display());
}

/// 启动清除:cold_start 加载落盘索引后调用——索引里残留「上次会话挂载、
/// 本次未挂载(或已停用)」的卷前缀,不清理则脏条目永久污染搜索结果
/// (落盘索引路径不重扫,无任何机制自然移除它们)。
pub fn purge_unmounted(state: &Arc<EngineState>, desired: &HashSet<PathBuf>) {
    let stale: HashSet<PathBuf> = state
        .dir_map
        .lock_fail_fast("dir_map")
        .keys()
        .filter_map(volume_root_of)
        .filter(|v| !desired.contains(v))
        .collect();
    // 有待清卷才物化:无卷变更的冷启动保持 Shared 常驻(零堆索引),不清白付
    if !stale.is_empty() {
        ensure_mutable(state);
    }
    for vol in stale {
        let removed = {
            let mut h = state.holder.write_fail_fast("holder");
            let Some(idx) = mutable_base_slot(&mut h) else {
                log::warn!("启动清除跳过(主索引非可变态): {}", vol.display());
                continue;
            };
            let mut map = state.dir_map.lock_fail_fast("dir_map");
            let removed = engine::scanner::remove_prefix(idx, &mut map, &vol);
            purge_overlay_after_prefix_removal(&mut h, &vol);
            removed
        };
        if removed > 0 {
            log::info!("启动清除未挂载卷前缀: {} 条目 {removed} 条", vol.display());
        }
    }
}
