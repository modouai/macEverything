//! 开机自启(SMAppService;Task 20,PoC-5 定稿结论落地)。
//!
//! PoC-5(`scripts/poc/poc5_autostart/`,结论见 `docs/specs/v2-ui/poc-results.md`):
//! - ad-hoc 签名 + `SMAppService.mainApp` 可用(无 sudo、无审批);三态
//!   notFound(3) → register → enabled(1) → unregister → notRegistered(0);
//! - 坑①:无 bundle 调 register **不报错**且注册出独立条目 —— 产品入口先校验
//!   bundle 标识(须为 `.app` 内的 `com.example.maceverything`),开发形态拒绝
//!   并提示「未打包」,不静默走这条陷阱;
//! - 坑②:条目身份键 = bundle 标识(路径、版本不参与);
//! - 坑③:注册态不进 launchctl 视图 —— 排障走系统设置「登录项」,勿查 launchctl;
//! - 坑④:未签名 arm64 进程直接 SIGKILL —— ad-hoc 签名是硬门槛(`scripts/bundle.sh`
//!   必签);签名无效时 register 报 kSMErrorInvalidSignature。
//!
//! 「首次启动默认注册 + 用户意图记忆」(plan Task 20 定案):启动时未打包 /
//! 用户曾主动关闭(数据目录标记文件存在)/ 已注册 → 跳过;其余注册。托盘取消
//! 勾选 → 写标记(下次启动不再默认注册);重新勾选 → 清标记。
//! 决策是纯函数 [`startup_decision`](单测),FFI 编排在 [`register_at_startup`]。
//!
//! 线程:调用点全在主线程(启动编排、托盘动作消费),本模块不做跨线程保证。

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use objc2_foundation::NSError;
use objc2_service_management::{
    kSMErrorAlreadyRegistered, kSMErrorInvalidSignature, kSMErrorJobNotFound,
    kSMErrorLaunchDeniedByUser, SMAppService, SMAppServiceStatus,
};

/// 产品 bundle 标识(与 `scripts/bundle.sh` 的 Info.plist 同值;PoC-5 坑②:条目身份键)。
pub const BUNDLE_ID: &str = "com.example.maceverything";

/// 用户主动关闭自启的标记文件名(数据目录内;存在 = 启动时不再默认注册)。
pub const OPT_OUT_FILE: &str = "autostart-off";

/// 注册态(PoC-5 实锁四值:notRegistered(0)/enabled(1)/requiresApproval(2)/notFound(3))。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 未注册(含注销后)。
    NotRegistered,
    /// 已注册且可登录拉起。
    Enabled,
    /// 已注册,但用户需在系统设置「登录项」中允许。
    RequiresApproval,
    /// 系统找不到对应条目(PoC-5 实测:首次运行、LaunchServices 尚未收录时为
    /// notFound(3);App 移到新路径后同样不可见)。决策侧按未注册处理 → 会重新注册。
    NotFound,
}

impl Status {
    /// 是否已注册(审批中算已注册:条目已在系统,只是等用户允许)。
    pub fn is_registered(self) -> bool {
        matches!(self, Status::Enabled | Status::RequiresApproval)
    }

    /// 中文说明(日志与提示用;四态各一行)。
    pub fn note(self) -> &'static str {
        match self {
            Status::Enabled => "已启用(系统设置「登录项」可见)",
            Status::RequiresApproval => "已注册,待用户在系统设置「登录项」中允许",
            Status::NotRegistered => "未注册(已关闭)",
            Status::NotFound => "未找到条目(注册尚未生效)",
        }
    }
}

/// 启动默认注册的决策(纯逻辑,单测;顺序即优先级:未打包 > 用户意图 > 已注册)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDecision {
    /// 未打包(.app 外;PoC-5 坑①:此形态注册会产独立条目,禁走)。
    SkipNotPackaged,
    /// 用户曾主动关闭(标记文件存在):尊重用户意图,不默认注册。
    SkipOptOut,
    /// 已注册:无需动作。
    SkipEnabled,
    /// 首次(或从未关闭过):默认注册(product §4)。
    Register,
}

/// 决策纯函数:`packaged` = 在 .app 内且 bundle 标识匹配;`opt_out` = 用户关闭标记存在;
/// `enabled` = 当前已注册。
pub fn startup_decision(packaged: bool, opt_out: bool, enabled: bool) -> StartupDecision {
    if !packaged {
        StartupDecision::SkipNotPackaged
    } else if opt_out {
        StartupDecision::SkipOptOut
    } else if enabled {
        StartupDecision::SkipEnabled
    } else {
        StartupDecision::Register
    }
}

/// 用户关闭标记的路径(数据目录内;启动默认注册与托盘开关共用)。
pub fn opt_out_path(data_dir: &Path) -> PathBuf {
    data_dir.join(OPT_OUT_FILE)
}

/// 当前进程是否运行在产品 bundle 内(bundle 标识精确匹配)。
/// 开发形态(`cargo run`)bundle 标识为 nil → false。
pub fn in_product_bundle() -> bool {
    let Some(id) = objc2_foundation::NSBundle::mainBundle().bundleIdentifier() else {
        return false;
    };
    id.to_string() == BUNDLE_ID
}

/// 查询注册状态(纯查询,不做打包校验;FFI 错误如实上抛)。
pub fn status() -> Result<Status> {
    // SAFETY: SMAppService.mainApp 为类方法,主线程调用;返回对象由 Retained 持有。
    let svc = unsafe { SMAppService::mainAppService() };
    status_of(&svc)
}

/// 已启用?(托盘「勾选态」口径:仅 enabled;requiresApproval 视为未启用——
/// 用户尚未在系统设置放行,登录时不会被拉起)。
pub fn is_enabled() -> Result<bool> {
    Ok(status()? == Status::Enabled)
}

/// 设置自启:`enabled=true` 注册,`false` 注销。幂等(目标态已达成直接 Ok,不重复下发)。
/// **须在 .app bundle 内调用**(开发形态 Err「未打包」;PoC-5 坑①)。
pub fn set_enabled(enabled: bool) -> Result<()> {
    if !in_product_bundle() {
        bail!("未打包:开机自启仅在 macEverything.app 内可用(打包请走 scripts/bundle.sh)");
    }
    // SAFETY: 同 status();register/unregister 为实例方法,主线程调用。
    let svc = unsafe { SMAppService::mainAppService() };
    let current = status_of(&svc)?;
    if enabled {
        if current.is_registered() {
            return Ok(()); // 幂等:已注册(含待审批)不重复调 register
        }
        unsafe { svc.registerAndReturnError() }.map_err(|err| sm_error("注册", &err))?;
    } else {
        if !current.is_registered() {
            return Ok(()); // 幂等:未注册无需注销
        }
        unsafe { svc.unregisterAndReturnError() }.map_err(|err| sm_error("注销", &err))?;
    }
    Ok(())
}

/// 启动时默认注册(product §4;plan Task 20 定案)。`data_dir` = 运行数据目录
/// (用户关闭标记所在)。跳过情形均留痕(开发形态提示「未打包」即此路径)。
pub fn register_at_startup(data_dir: &Path) -> Result<()> {
    let packaged = in_product_bundle();
    // 未打包不进状态查询:PoC-5 坑① 下 status 无意义,直接按未打包短路。
    // 命中已注册分支时把**精确状态**(enabled / requiresApproval)记入日志:
    // 两态都算「已注册」,但要不要提示用户去系统设置放行只能看精确态。
    let (registered, note) = if packaged {
        match status() {
            Ok(s) => (s.is_registered(), s.note()),
            Err(err) => {
                // 查询失败按未注册处理(下一步 register 有幂等兜底),不静默。
                log::warn!("开机自启:状态查询失败,按未注册处理: {err:#}");
                (false, "状态查询失败")
            }
        }
    } else {
        (false, "未打包")
    };
    match startup_decision(packaged, opt_out_path(data_dir).exists(), registered) {
        StartupDecision::SkipNotPackaged => {
            log::info!("开机自启:开发形态(未打包 .app),跳过默认注册");
            Ok(())
        }
        StartupDecision::SkipOptOut => {
            log::info!("开机自启:用户曾主动关闭,尊重用户意图不默认注册");
            Ok(())
        }
        StartupDecision::SkipEnabled => {
            log::info!("开机自启:已在登录项中注册 —— {note}");
            Ok(())
        }
        StartupDecision::Register => {
            set_enabled(true)?;
            let note = status().map(|s| s.note()).unwrap_or("已注册");
            log::info!("开机自启:默认注册完成 —— {note}");
            Ok(())
        }
    }
}

/// 用户经托盘/设置页主动开关(与启动默认注册区分:此处写/清「用户关闭」标记)。
///
/// 返回**操作后查询到的真实注册态**(注册/注销调用本身成功不代表目标态达成:
/// 例如启用后可能停在 `RequiresApproval`,登录时不会拉起)。调用方须按 `Status`
/// 决定开关显示与托盘勾选,勿用请求的 `enabled` 反推。注册/注销失败或状态查询
/// 失败 → Err(调用方复原勾选/开关);标记读写失败不回滚注册(注册已生效),记
/// warn 后仍返回 Ok。
pub fn apply_user_choice(data_dir: &Path, enabled: bool) -> Result<Status> {
    set_enabled(enabled)?;
    let marker = opt_out_path(data_dir);
    let marker_result = if enabled {
        clear_marker(&marker)
    } else {
        write_marker(&marker)
    };
    if let Err(err) = marker_result {
        // 注册已生效,不回滚;标记失败影响的是「下次启动默认注册」的意图记忆。
        log::warn!("开机自启:用户意图标记写入失败({}):{err}", marker.display());
    }
    let status = status().context("开机自启:操作后状态查询失败")?;
    log::info!("开机自启:操作后状态 —— {}", status.note());
    Ok(status)
}

/// 写「用户关闭」标记(建父目录;幂等覆盖)。
fn write_marker(marker: &Path) -> std::io::Result<()> {
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(marker, b"")
}

/// 清「用户关闭」标记(不存在视为成功:用户重新勾选时标记可能本就不在)。
fn clear_marker(marker: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(marker) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// 读状态并映射为 [`Status`];未知原始值如实报错(系统新态不静默兜底)。
fn status_of(svc: &SMAppService) -> Result<Status> {
    // SAFETY: status 为只读属性访问;svc 由 Retained 保活。
    let raw = unsafe { svc.status() };
    if raw == SMAppServiceStatus::NotRegistered {
        Ok(Status::NotRegistered)
    } else if raw == SMAppServiceStatus::Enabled {
        Ok(Status::Enabled)
    } else if raw == SMAppServiceStatus::RequiresApproval {
        Ok(Status::RequiresApproval)
    } else if raw == SMAppServiceStatus::NotFound {
        Ok(Status::NotFound)
    } else {
        bail!("SMAppService 返回未知状态({})", raw.0)
    }
}

/// SM 错误 → 带码与可操作提示的 anyhow 错误(用户可读的 localizedDescription + 常见码提示)。
fn sm_error(action: &str, err: &NSError) -> anyhow::Error {
    let code = u32::try_from(err.code()).ok();
    // 注:SMErrors 常量为小写(Apple 头文件原样),case 比较放守卫表达式——
    // 放进模式会触发 non_upper_case_globals(lint 只查模式位置)。
    let hint = match code {
        Some(c) if c == kSMErrorInvalidSignature => {
            " —— App 签名无效:先用 scripts/bundle.sh 重新打包(ad-hoc 签名是自启硬门槛)"
        }
        Some(c) if c == kSMErrorLaunchDeniedByUser => {
            " —— 用户已在系统设置中拒绝:可在系统设置「登录项」中重新允许"
        }
        Some(c) if c == kSMErrorAlreadyRegistered => " —— 条目已在登录项中注册",
        Some(c) if c == kSMErrorJobNotFound => " —— 登录项中无此条目",
        _ => "",
    };
    anyhow!(
        "{action}登录项失败:{}{hint}(code={:?})",
        err.localizedDescription(),
        code
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 决策真值表:未打包 > 用户意图 > 已注册 > 注册(顺序即优先级)。
    #[test]
    fn startup_decision_priority() {
        use StartupDecision::*;
        // 未打包:无论标记与注册态,一律跳过(且优先于其他判断)。
        assert_eq!(startup_decision(false, false, false), SkipNotPackaged);
        assert_eq!(startup_decision(false, true, true), SkipNotPackaged);
        // 已打包:用户关闭标记优先于「未注册」→ 不默认注册。
        assert_eq!(startup_decision(true, true, false), SkipOptOut);
        assert_eq!(startup_decision(true, true, true), SkipOptOut);
        // 已注册:无需动作。
        assert_eq!(startup_decision(true, false, true), SkipEnabled);
        // 首次(打包、无标记、未注册):默认注册。
        assert_eq!(startup_decision(true, false, false), Register);
    }

    /// 标记路径落在数据目录，文件名稳定(启动默认注册与托盘开关同源)。
    #[test]
    fn opt_out_path_is_data_dir_marker() {
        let p = opt_out_path(Path::new("/tmp/mce-data"));
        assert_eq!(p, PathBuf::from("/tmp/mce-data/autostart-off"));
        assert_eq!(p.file_name().unwrap(), OPT_OUT_FILE);
    }

    /// 标记读写往返(apply_user_choice 的两条标记路径):写建父目录 → 存在;
    /// 清 → 不存在;清「本就不存在」视为成功(用户重新勾选时标记可能不在)。
    #[test]
    fn opt_out_marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mce-autostart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marker = opt_out_path(&dir);
        assert!(!marker.exists(), "初始无标记");

        // write_marker 建父目录(数据目录可能尚未由引擎创建)。
        write_marker(&marker).unwrap();
        assert!(marker.exists(), "写入后标记存在");

        clear_marker(&marker).unwrap();
        assert!(!marker.exists(), "清除后标记不存在");
        clear_marker(&marker).unwrap(); // 重复清 = 从不存在清:幂等不报错

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 状态说明四态各一行(日志文案契约;不含空串)。
    #[test]
    fn status_notes_are_distinct() {
        let all = [
            Status::Enabled,
            Status::RequiresApproval,
            Status::NotRegistered,
            Status::NotFound,
        ];
        for s in all {
            assert!(!s.note().is_empty(), "{s:?} 缺说明");
        }
        // 已注册口径:enabled 与 requiresApproval 都算。
        assert!(Status::Enabled.is_registered());
        assert!(Status::RequiresApproval.is_registered());
        assert!(!Status::NotRegistered.is_registered());
        assert!(!Status::NotFound.is_registered());
    }

    /// 开发形态守卫:`.app` 外 set_enabled 必须拒绝(PoC-5 坑①:无 bundle 注册
    /// 不报错且产独立条目,产品入口须拦住)。`cargo test` 的测试宿主不在 bundle 内。
    #[test]
    fn set_enabled_outside_bundle_refused() {
        assert!(!in_product_bundle(), "测试宿主不应在 .app bundle 内");
        let err = set_enabled(true).unwrap_err();
        assert!(
            err.to_string().contains("未打包"),
            "拒绝理由须说明未打包,实际:{err}"
        );
        let err = set_enabled(false).unwrap_err();
        assert!(err.to_string().contains("未打包"), "注销同样须拒绝");
    }
}
