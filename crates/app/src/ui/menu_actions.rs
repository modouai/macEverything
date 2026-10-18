//! 右键菜单动作(Task 18;product §2 菜单四语义表)。
//!
//! 两段结构:
//!
//! - 纯逻辑(`menu_items` / `count_types` / `paths_text` / tag 映射,可测);
//! - 动作执行:`plan`(动作 → 命令/载荷步骤,纯数据可测)产出计划,`execute`
//!   只做落地(open / pbcopy 数组参数调起,禁 shell 拼接——platform-compat 红线)。
//!
//! 菜单项按选中类型收敛(product §2 表):
//! - 纯文件:打开 / 在访达中打开 / 在访达中显示 / 拷贝路径(四项);
//! - 纯目录:打开(即访达进入)/ 在访达中显示 / 拷贝路径(无「在访达中打开」,
//!   目录的「打开」即此语义,不重复出现);
//! - 混选:四项齐全(两者语义并集)。
//!
//! 自持实现(不并入 palette::open_path):那是「单件 + reveal 布尔」语义,本处是
//! 「批量 + 四动作」(如「在访达中打开」= 打开父目录,无对应布尔位),合流会把两个
//! 调用面揉成参数迷宫(plan Task 18 注释「menu_actions 自持更干净」)。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

use crate::i18n;

/// 四动作(product §2「菜单项」列)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// 打开:文件 = 默认应用打开;目录 = 访达进入(两者同为 `open <path>`)。
    Open,
    /// 在访达中打开:打开所在目录(进入,不选中)。
    OpenInFinderDir,
    /// 在访达中显示:父目录中选中该项(`open -R`;逐项)。
    RevealInFinder,
    /// 拷贝路径:全部路径逐行复制(`pbcopy`)。
    CopyPaths,
}

impl MenuAction {
    /// NSMenu tag(1 起,PoC-3 同口径;避开 0——NSMenuItem 默认 tag)。
    pub fn tag(self) -> i64 {
        match self {
            Self::Open => 1,
            Self::OpenInFinderDir => 2,
            Self::RevealInFinder => 3,
            Self::CopyPaths => 4,
        }
    }

    /// tag → 动作(未知 tag = None;调用方留痕,不静默)。
    pub fn from_tag(tag: i64) -> Option<Self> {
        match tag {
            1 => Some(Self::Open),
            2 => Some(Self::OpenInFinderDir),
            3 => Some(Self::RevealInFinder),
            4 => Some(Self::CopyPaths),
            _ => None,
        }
    }
}

/// 按选中类型收敛的菜单项(product §2 表逐行一致;标题即菜单展示文案)。
///
/// `files`/`dirs` = 选中行的文件/目录计数(`count_types`)。空选(0,0)返回空集
/// (调用方 = 右键落在行上,保证 ≥1;0/0 属调用方缺陷,防御为空菜单不弹)。
pub fn menu_items(files: usize, dirs: usize) -> Vec<(&'static str, MenuAction)> {
    if files == 0 && dirs == 0 {
        return Vec::new();
    }
    let s = i18n::s();
    let mut v = vec![(s.menu_open, MenuAction::Open)];
    if files > 0 {
        v.push((s.menu_open_in_finder, MenuAction::OpenInFinderDir));
    }
    v.push((s.menu_show_in_finder, MenuAction::RevealInFinder));
    v.push((s.menu_copy_path, MenuAction::CopyPaths));
    v
}

/// 选中路径分类计数(文件数, 目录数)。
///
/// `HitEntry.is_dir` 是索引快照,右键动作要求当前文件系统状态,故仍现查元数据;
/// 取不到(已删/无权限)按文件计——保守:不出现目录专属语义项,执行时统一做
/// 存在性前置校验并把失败落状态栏,不静默。
pub fn count_types(paths: &[String]) -> (usize, usize) {
    let mut files = 0;
    let mut dirs = 0;
    for p in paths {
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => dirs += 1,
            _ => files += 1,
        }
    }
    (files, dirs)
}

/// 拷贝路径载荷:逐行 join(纯逻辑;不带尾换行,终端粘贴不触发执行)。
pub fn paths_text(paths: &[String]) -> String {
    paths.join("\n")
}

/// 执行计划的一步(纯数据;`plan` 产出、`execute` 落地——命令面因此可测,
/// 测试不触发真实访达/剪贴板副作用)。
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// 后台 spawn `program args...`(不等待)。
    Run {
        program: &'static str,
        args: Vec<String>,
    },
    /// 标准输入写 `text`(pbcopy;不经 shell)。
    Pipe { text: String },
}

/// 动作 → 执行计划(纯逻辑,可测)。批量语义按 product §2 表;
/// 先整批前置校验(存在性)再产出步骤——校验失败整批不执行,
/// 避免陈旧索引下「部分打开部分报错」的分裂状态。
fn plan(action: MenuAction, paths: &[String]) -> Result<Vec<Step>> {
    match action {
        MenuAction::Open => {
            // 文件 = 默认应用打开;目录 = 访达进入;同为 `open <path>`,逐项
            for p in paths {
                ensure_exists(p)?;
            }
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec![p.clone()],
                })
                .collect())
        }
        MenuAction::OpenInFinderDir => {
            let mut targets: Vec<String> = Vec::new();
            for p in paths {
                let parent = Path::new(p)
                    .parent()
                    .ok_or_else(|| anyhow!("{}: {p}", i18n::s().menu_locate_failed))?;
                let parent = parent.to_string_lossy().into_owned();
                ensure_exists(&parent)?;
                if !targets.contains(&parent) {
                    targets.push(parent); // 同目录去重(多项同目录只开一次)
                }
            }
            Ok(targets
                .into_iter()
                .map(|t| Step::Run {
                    program: "open",
                    args: vec![t],
                })
                .collect())
        }
        MenuAction::RevealInFinder => {
            for p in paths {
                ensure_exists(p)?;
            }
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec!["-R".to_string(), p.clone()],
                })
                .collect())
        }
        MenuAction::CopyPaths => Ok(vec![Step::Pipe {
            text: paths_text(paths),
        }]),
    }
}

/// 执行动作(plan → 逐步骤落地)。失败返回可展示提示(调用方落状态栏)。
pub fn execute(action: MenuAction, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        // 调用方保证非空(菜单仅在 ≥1 行选中时弹出);空集防御:不发无参 `open`
        // (无参 open 会新开 Finder 窗口——非预期副作用)。
        log::warn!("菜单动作 {action:?}:路径集为空,忽略");
        return Ok(());
    }
    let steps = plan(action, paths)?;
    for step in &steps {
        match step {
            Step::Run { program, args } => spawn_run(program, args)?,
            Step::Pipe { text } => copy_to_clipboard(text)?,
        }
    }
    log::info!(
        "菜单动作 {action:?}:{} 项 / {} 步已发起",
        paths.len(),
        steps.len()
    );
    Ok(())
}

/// 存在性前置校验(失败文案与 palette::open_path 一致——同一根因:索引陈旧)。
fn ensure_exists(path: &str) -> Result<()> {
    if std::fs::metadata(path).is_err() {
        return Err(anyhow!("{}", i18n::s().menu_file_missing));
    }
    Ok(())
}

/// 后台 spawn(不等待;退出码异常留痕——open 非零 = 系统未受理)。
fn spawn_run(program: &'static str, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    match cmd.spawn() {
        Ok(mut child) => {
            let what = format!("{program} {}", args.join(" "));
            std::thread::spawn(move || match child.wait() {
                Ok(st) if !st.success() => log::warn!("{program} 退出异常({st}):{what}"),
                Ok(_) => {}
                Err(e) => log::warn!("等待 {program} 退出失败:{e}"),
            });
            Ok(())
        }
        Err(e) => Err(anyhow!("{}: {e}", i18n::s().menu_open_failed)),
    }
}

/// 写剪贴板(pbcopy + stdin 写入,数组参数不经 shell——platform-compat 红线)。
fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("{}: {e}", i18n::s().menu_copy_failed))?;
    // stdin 于本作用域末 drop → EOF(pbcopy 读到 EOF 才落剪贴板)
    let write_res = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    let wait_res = child.wait();
    if let Err(e) = write_res {
        return Err(anyhow!("{}: {e}", i18n::s().menu_copy_failed));
    }
    match wait_res {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(anyhow!("{} (pbcopy exit {st})", i18n::s().menu_copy_failed)),
        Err(e) => Err(anyhow!("{}: {e}", i18n::s().menu_copy_failed)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_lang, Lang};

    fn names(items: &[(&'static str, MenuAction)]) -> Vec<&'static str> {
        items.iter().map(|(t, _)| *t).collect()
    }

    /// product §2 表:纯文件四项(含「在访达中打开」)。
    #[test]
    fn menu_items_files_four() {
        let items = menu_items(1, 0);
        assert_eq!(
            names(&items),
            ["打开", "在访达中打开", "在访达中显示", "拷贝路径"]
        );
        assert_eq!(
            items.iter().map(|(_, a)| *a).collect::<Vec<_>>(),
            [
                MenuAction::Open,
                MenuAction::OpenInFinderDir,
                MenuAction::RevealInFinder,
                MenuAction::CopyPaths
            ]
        );
        assert_eq!(menu_items(5, 0).len(), 4, "多文件同形");
    }

    /// 文案随语言切换:英文菜单项逐字(muda 原生菜单直接取这些标题)。
    #[test]
    fn menu_items_localized_in_english() {
        with_lang(Lang::En, || {
            let items = menu_items(1, 0);
            assert_eq!(
                names(&items),
                ["Open", "Open in Finder", "Show in Finder", "Copy Path"]
            );
        });
    }

    /// product §2 表:纯目录三项,无「在访达中打开」(目录的「打开」即此语义)。
    #[test]
    fn menu_items_dirs_three() {
        let items = menu_items(0, 1);
        assert_eq!(names(&items), ["打开", "在访达中显示", "拷贝路径"]);
        assert!(
            !items.iter().any(|(_, a)| *a == MenuAction::OpenInFinderDir),
            "目录无「在访达中打开」"
        );
        assert_eq!(menu_items(0, 7).len(), 3, "多目录同形");
    }

    /// product §2 表:混选 = 并集四项齐全。
    #[test]
    fn menu_items_mixed_four() {
        let items = menu_items(1, 1);
        assert_eq!(
            names(&items),
            ["打开", "在访达中打开", "在访达中显示", "拷贝路径"]
        );
        assert_eq!(menu_items(3, 2).len(), 4);
    }

    /// 空选(0,0,理论不出现)返回空集:不弹空菜单(调用方按空集跳过弹出)。
    #[test]
    fn menu_items_empty_is_empty() {
        assert!(menu_items(0, 0).is_empty());
    }

    /// tag 往返(NSMenu tag ↔ 动作;未知 tag 不静默映射)。
    #[test]
    fn tag_roundtrip() {
        for a in [
            MenuAction::Open,
            MenuAction::OpenInFinderDir,
            MenuAction::RevealInFinder,
            MenuAction::CopyPaths,
        ] {
            assert_eq!(MenuAction::from_tag(a.tag()), Some(a));
        }
        assert_eq!(
            MenuAction::from_tag(0),
            None,
            "0 是 NSMenuItem 默认 tag,不映射"
        );
        assert_eq!(MenuAction::from_tag(9), None);
    }

    /// 拷贝载荷:逐行 join,单条与空集形态钉住。
    #[test]
    fn paths_text_line_joins() {
        let p = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert_eq!(paths_text(&p(&["/a/b.txt"])), "/a/b.txt", "单条无尾换行");
        assert_eq!(paths_text(&p(&["/a", "/b/c"])), "/a\n/b/c", "多条逐行");
        assert_eq!(paths_text(&[]), "", "空集空串");
    }

    /// 类型计数:文件/目录分流;取不到元数据按文件计(保守)。
    #[test]
    fn count_types_classifies() {
        let base = std::env::temp_dir().join(format!("mce-menu-actions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("dir_a")).unwrap();
        std::fs::write(base.join("file_a.txt"), b"x").unwrap();
        let paths = |v: &[&str]| -> Vec<String> {
            v.iter()
                .map(|n| base.join(n).to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(count_types(&paths(&["file_a.txt"])), (1, 0));
        assert_eq!(count_types(&paths(&["dir_a"])), (0, 1));
        assert_eq!(count_types(&paths(&["file_a.txt", "dir_a"])), (1, 1));
        assert_eq!(count_types(&paths(&["missing"])), (1, 0), "取不到按文件计");
        assert_eq!(count_types(&[]), (0, 0));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 动作 → 命令面(plan 层):四项动作的确切程序/参数/载荷(与 product 表逐项对应;
    /// 测试不真开访达/剪贴板——命令面在 plan 内可测,execute 只做落地)。
    #[test]
    fn plan_command_lines() {
        let base = std::env::temp_dir().join(format!("mce-menu-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("dir_a")).unwrap();
        let f1 = base.join("a.txt");
        let f2 = base.join("b.txt");
        let g1 = base.join("dir_a/c.txt");
        std::fs::write(&f1, b"x").unwrap();
        std::fs::write(&f2, b"x").unwrap();
        std::fs::write(&g1, b"x").unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let run = |args: Vec<String>| Step::Run {
            program: "open",
            args,
        };

        // 打开:逐项 `open <path>`(文件/目录同一命令:默认应用 / 访达进入)
        assert_eq!(
            plan(MenuAction::Open, &[s(&f1)]).unwrap(),
            vec![run(vec![s(&f1)])]
        );
        assert_eq!(
            plan(MenuAction::Open, &[s(&f1), s(&f2)]).unwrap(),
            vec![run(vec![s(&f1)]), run(vec![s(&f2)])],
            "多选逐项打开"
        );

        // 在访达中打开:`open <父目录>`;同目录去重
        assert_eq!(
            plan(MenuAction::OpenInFinderDir, &[s(&f1), s(&f2), s(&g1)]).unwrap(),
            vec![run(vec![s(&base)]), run(vec![s(&base.join("dir_a"))])],
            "同目录只开一次"
        );

        // 在访达中显示:`open -R <path>` 逐项
        assert_eq!(
            plan(MenuAction::RevealInFinder, &[s(&f1), s(&f2)]).unwrap(),
            vec![
                run(vec!["-R".to_string(), s(&f1)]),
                run(vec!["-R".to_string(), s(&f2)])
            ]
        );

        // 拷贝路径:单步 Pipe,载荷逐行 join
        assert_eq!(
            plan(MenuAction::CopyPaths, &[s(&f1), s(&f2)]).unwrap(),
            vec![Step::Pipe {
                text: format!("{}\n{}", s(&f1), s(&f2))
            }]
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 存在性前置校验:陈旧索引路径 → 可展示错误,且不发动作(Open 不 spawn)。
    #[test]
    fn execute_missing_path_fails_before_spawn() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        let err = execute(MenuAction::Open, std::slice::from_ref(&missing)).unwrap_err();
        assert!(
            err.to_string().contains("文件不存在"),
            "错误文案应可展示:{err}"
        );
        let err = execute(MenuAction::RevealInFinder, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
        // 「在访达中打开」:父目录不存在同样前置拦下(路径父级也须不存在,
        // 否则会真的 spawn `open`——测试不得产生打开访达的副作用)
        let deep = "/tmp/mce-menu-actions-不存在-000000/sub/file.txt".to_string();
        let err = execute(MenuAction::OpenInFinderDir, &[deep]).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
    }

    /// 执行错误随语言切换(状态栏直接展示;中文文案由上方用例钉住)。
    #[test]
    fn execute_missing_path_error_is_localized() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        with_lang(Lang::En, || {
            let err = execute(MenuAction::Open, std::slice::from_ref(&missing)).unwrap_err();
            assert!(
                err.to_string()
                    .contains("File not found; the index may be out of date"),
                "错误文案应随语言切换:{err}"
            );
        });
    }

    /// 空集防御:不发无参 `open`(无参 open 会新开 Finder 窗口),返回 Ok。
    #[test]
    fn execute_empty_paths_is_noop() {
        execute(MenuAction::Open, &[]).unwrap();
        execute(MenuAction::CopyPaths, &[]).unwrap();
    }
}
