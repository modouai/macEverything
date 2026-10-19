//! 右键菜单动作(v2.2/v2.3;product §2 菜单表)。
//!
//! 两段结构:
//!
//! - 纯逻辑(`menu_entries` / `names_text` / 存在性校验 / tag 映射,可测);
//! - shell 动作执行:`plan`(动作 -> 命令/载荷步骤,纯数据可测)产出计划,`execute`
//!   只做落地(open / pbcopy 数组参数调起,禁 shell 拼接——platform-compat 红线)。
//!
//! 菜单最终形态(用户反馈后):打开 / 在访达中显示 / 快速查看 / 分隔线 /
//! 拷贝文件 / 拷贝路径 / 拷贝文件名(拷贝三项平铺,无二级菜单)。
//! 快速查看与拷贝文件是平台动作,不进 shell plan;⌘C/⌘⇧C 由 results.rs 处理。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

use crate::i18n;
use crate::platform::{MenuEntry, MenuShortcut};

/// 菜单动作(tag 为 NSMenu 进程内标识,无持久化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    RevealInFinder,
    QuickLook,
    CopyFile,
    CopyPaths,
    CopyFileName,
}

impl MenuAction {
    pub fn tag(self) -> i64 {
        match self {
            Self::Open => 1,
            Self::RevealInFinder => 2,
            Self::CopyPaths => 3,
            Self::QuickLook => 4,
            Self::CopyFile => 5,
            Self::CopyFileName => 6,
        }
    }

    pub fn from_tag(tag: i64) -> Option<Self> {
        match tag {
            1 => Some(Self::Open),
            2 => Some(Self::RevealInFinder),
            3 => Some(Self::CopyPaths),
            4 => Some(Self::QuickLook),
            5 => Some(Self::CopyFile),
            6 => Some(Self::CopyFileName),
            _ => None,
        }
    }

    /// 平台动作返回 None,由 ui 直接调用 `platform` facade。
    pub fn shell(self) -> Option<ShellAction> {
        match self {
            Self::Open => Some(ShellAction::Open),
            Self::RevealInFinder => Some(ShellAction::RevealInFinder),
            Self::CopyPaths => Some(ShellAction::CopyPaths),
            Self::CopyFileName => Some(ShellAction::CopyFileName),
            Self::QuickLook | Self::CopyFile => None,
        }
    }
}

/// 可计划/落地的 shell 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    Open,
    RevealInFinder,
    CopyPaths,
    CopyFileName,
}

/// 菜单结构(每次右键重建;文案取当前语言;拷贝三项平铺)。
pub fn menu_entries() -> Vec<MenuEntry> {
    let s = i18n::s();
    vec![
        MenuEntry::Item {
            title: s.menu_open,
            tag: MenuAction::Open.tag(),
            enabled: true,
            shortcut: None,
        },
        MenuEntry::Item {
            title: s.menu_show_in_finder,
            tag: MenuAction::RevealInFinder.tag(),
            enabled: true,
            shortcut: None,
        },
        MenuEntry::Item {
            title: s.menu_quick_look,
            tag: MenuAction::QuickLook.tag(),
            enabled: true,
            shortcut: None,
        },
        MenuEntry::Separator,
        MenuEntry::Item {
            title: s.menu_copy_file,
            tag: MenuAction::CopyFile.tag(),
            enabled: true,
            shortcut: Some(MenuShortcut {
                key: "c",
                command: true,
                shift: false,
            }),
        },
        MenuEntry::Item {
            title: s.menu_copy_path,
            tag: MenuAction::CopyPaths.tag(),
            enabled: true,
            shortcut: Some(MenuShortcut {
                key: "c",
                command: true,
                shift: true,
            }),
        },
        MenuEntry::Item {
            title: s.menu_copy_name,
            tag: MenuAction::CopyFileName.tag(),
            enabled: true,
            shortcut: None,
        },
    ]
}

/// 文件名载荷:取末段;根目录等无末段时回退完整路径;逐行 join 无尾换行。
pub fn names_text(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 整批存在性校验(Quick Look / 拷贝文件/文件名执行前调用)。
pub fn ensure_existing(paths: &[String]) -> Result<()> {
    for p in paths {
        ensure_exists(p)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Run {
        program: &'static str,
        args: Vec<String>,
    },
    Pipe {
        text: String,
    },
}

fn plan(action: ShellAction, paths: &[String]) -> Result<Vec<Step>> {
    match action {
        ShellAction::Open => {
            ensure_existing(paths)?;
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec![p.clone()],
                })
                .collect())
        }
        ShellAction::RevealInFinder => {
            ensure_existing(paths)?;
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec!["-R".to_string(), p.clone()],
                })
                .collect())
        }
        ShellAction::CopyPaths => Ok(vec![Step::Pipe {
            text: paths.join("\n"),
        }]),
        ShellAction::CopyFileName => {
            ensure_existing(paths)?;
            Ok(vec![Step::Pipe {
                text: names_text(paths),
            }])
        }
    }
}

/// 拷贝步骤失败文案(CopyFileName 与路径拷贝分别提示)。
fn pipe_failure_text(action: ShellAction) -> &'static str {
    match action {
        ShellAction::CopyFileName => i18n::s().copy_name_failed,
        _ => i18n::s().menu_copy_failed,
    }
}

pub fn execute(action: ShellAction, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        log::warn!("菜单动作 {action:?}:路径集为空,忽略");
        return Ok(());
    }
    let steps = plan(action, paths)?;
    for step in &steps {
        match step {
            Step::Run { program, args } => spawn_run(program, args)?,
            Step::Pipe { text } => {
                copy_to_clipboard(text, pipe_failure_text(action))?;
            }
        }
    }
    log::info!(
        "菜单动作 {action:?}:{} 项 / {} 步已发起",
        paths.len(),
        steps.len()
    );
    Ok(())
}

fn ensure_exists(path: &str) -> Result<()> {
    if std::fs::metadata(path).is_err() {
        return Err(anyhow!("{}", i18n::s().menu_file_missing));
    }
    Ok(())
}

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

fn copy_to_clipboard(text: &str, failure: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("{failure}: {e}"))?;
    let write_res = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    let wait_res = child.wait();
    if let Err(e) = write_res {
        return Err(anyhow!("{failure}: {e}"));
    }
    match wait_res {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(anyhow!("{failure} (pbcopy exit {st})")),
        Err(e) => Err(anyhow!("{failure}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_lang, Lang};
    use crate::platform::{MenuEntry, MenuShortcut};

    const CMD_C: Option<MenuShortcut> = Some(MenuShortcut {
        key: "c",
        command: true,
        shift: false,
    });
    const CMD_SHIFT_C: Option<MenuShortcut> = Some(MenuShortcut {
        key: "c",
        command: true,
        shift: true,
    });

    fn assert_item(entry: &MenuEntry, expected_title: &str, expected_tag: i64) {
        match entry {
            MenuEntry::Item {
                title,
                tag,
                enabled,
                ..
            } => {
                assert_eq!(*title, expected_title);
                assert_eq!(*tag, expected_tag);
                assert!(enabled);
            }
            other => panic!("期望菜单项,得到 {other:?}"),
        }
    }

    fn assert_shortcut(entry: &MenuEntry, expected: Option<MenuShortcut>) {
        match entry {
            MenuEntry::Item { shortcut, .. } => assert_eq!(*shortcut, expected),
            other => panic!("期望菜单项,得到 {other:?}"),
        }
    }

    /// 用户反馈后的最终形态:拷贝三项平铺到一级,并显示快捷键。
    #[test]
    fn menu_entries_final_shape() {
        let items = menu_entries();
        assert_eq!(items.len(), 7);
        assert_item(&items[0], "打开", MenuAction::Open.tag());
        assert_shortcut(&items[0], None);
        assert_item(&items[1], "在访达中显示", MenuAction::RevealInFinder.tag());
        assert_shortcut(&items[1], None);
        assert_item(&items[2], "快速查看", MenuAction::QuickLook.tag());
        assert_shortcut(&items[2], None);
        assert_eq!(items[3], MenuEntry::Separator);
        assert_item(&items[4], "拷贝文件", MenuAction::CopyFile.tag());
        assert_shortcut(&items[4], CMD_C);
        assert_item(&items[5], "拷贝路径", MenuAction::CopyPaths.tag());
        assert_shortcut(&items[5], CMD_SHIFT_C);
        assert_item(&items[6], "拷贝文件名", MenuAction::CopyFileName.tag());
        assert_shortcut(&items[6], None);
    }

    #[test]
    fn tag_roundtrip() {
        for a in [
            MenuAction::Open,
            MenuAction::RevealInFinder,
            MenuAction::QuickLook,
            MenuAction::CopyFile,
            MenuAction::CopyPaths,
            MenuAction::CopyFileName,
        ] {
            assert_eq!(MenuAction::from_tag(a.tag()), Some(a));
        }
        assert_eq!(MenuAction::Open.tag(), 1);
        assert_eq!(MenuAction::RevealInFinder.tag(), 2);
        assert_eq!(MenuAction::CopyPaths.tag(), 3);
        assert_eq!(MenuAction::QuickLook.tag(), 4);
        assert_eq!(MenuAction::CopyFile.tag(), 5);
        assert_eq!(MenuAction::CopyFileName.tag(), 6);
        assert_eq!(MenuAction::from_tag(0), None);
        assert_eq!(MenuAction::from_tag(7), None);
    }

    #[test]
    fn menu_entries_localized_in_english() {
        with_lang(Lang::En, || {
            let items = menu_entries();
            assert_item(&items[0], "Open", 1);
            assert_item(&items[1], "Show in Finder", 2);
            assert_item(&items[2], "Quick Look", 4);
            assert_eq!(items[3], MenuEntry::Separator);
            assert_item(&items[4], "Copy File", 5);
            assert_shortcut(&items[4], CMD_C);
            assert_item(&items[5], "Copy Path", 3);
            assert_shortcut(&items[5], CMD_SHIFT_C);
            assert_item(&items[6], "Copy File Name", 6);
        });
    }

    #[test]
    fn names_text_line_joins_and_falls_back_to_path() {
        let p = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert_eq!(names_text(&p(&["/a/b.txt"])), "b.txt");
        assert_eq!(names_text(&p(&["/a/b.txt", "/c/d/e.pdf"])), "b.txt\ne.pdf");
        assert_eq!(
            names_text(&p(&["/"])),
            "/",
            "根目录无 file_name 时回退完整路径"
        );
        assert_eq!(names_text(&[]), "");
    }

    #[test]
    fn platform_actions_have_no_shell_plan() {
        assert_eq!(MenuAction::QuickLook.shell(), None);
        assert_eq!(MenuAction::CopyFile.shell(), None);
        assert_eq!(MenuAction::Open.shell(), Some(ShellAction::Open));
        assert_eq!(
            MenuAction::RevealInFinder.shell(),
            Some(ShellAction::RevealInFinder)
        );
        assert_eq!(MenuAction::CopyPaths.shell(), Some(ShellAction::CopyPaths));
        assert_eq!(
            MenuAction::CopyFileName.shell(),
            Some(ShellAction::CopyFileName)
        );
    }

    #[test]
    fn pipe_failure_text_matches_action_and_language() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(
                pipe_failure_text(ShellAction::CopyFileName),
                "拷贝文件名失败"
            );
            assert_eq!(pipe_failure_text(ShellAction::CopyPaths), "拷贝路径失败");
        });
        with_lang(Lang::En, || {
            assert_eq!(
                pipe_failure_text(ShellAction::CopyFileName),
                "Failed to copy file name"
            );
            assert_eq!(
                pipe_failure_text(ShellAction::CopyPaths),
                "Failed to copy path"
            );
        });
    }

    #[test]
    fn ensure_existing_rejects_missing() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        let err = ensure_existing(std::slice::from_ref(&missing)).unwrap_err();
        assert!(
            err.to_string().contains("文件不存在"),
            "错误文案应可展示:{err}"
        );
        ensure_existing(&[]).unwrap();
    }

    #[test]
    fn ensure_existing_rejects_mixed_batch() {
        let base = std::env::temp_dir().join(format!("mce-menu-ensure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let existing = base.join("exists.txt");
        std::fs::write(&existing, b"x").unwrap();
        let missing = base.join("missing.txt");
        let paths = vec![
            existing.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ];
        let err = ensure_existing(&paths).unwrap_err();
        assert!(
            err.to_string().contains("文件不存在"),
            "错误文案应可展示:{err}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn plan_command_lines() {
        let base = std::env::temp_dir().join(format!("mce-menu-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let f1 = base.join("a.txt");
        let f2 = base.join("b.txt");
        std::fs::write(&f1, b"x").unwrap();
        std::fs::write(&f2, b"x").unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let run = |args: Vec<String>| Step::Run {
            program: "open",
            args,
        };

        assert_eq!(
            plan(ShellAction::Open, &[s(&f1)]).unwrap(),
            vec![run(vec![s(&f1)])]
        );
        assert_eq!(
            plan(ShellAction::Open, &[s(&f1), s(&f2)]).unwrap(),
            vec![run(vec![s(&f1)]), run(vec![s(&f2)])],
            "多选逐项打开"
        );
        assert_eq!(
            plan(ShellAction::RevealInFinder, &[s(&f1), s(&f2)]).unwrap(),
            vec![
                run(vec!["-R".to_string(), s(&f1)]),
                run(vec!["-R".to_string(), s(&f2)])
            ]
        );
        assert_eq!(
            plan(ShellAction::CopyPaths, &[s(&f1), s(&f2)]).unwrap(),
            vec![Step::Pipe {
                text: format!("{}\n{}", s(&f1), s(&f2))
            }]
        );
        assert_eq!(
            plan(ShellAction::CopyFileName, &[s(&f1), s(&f2)]).unwrap(),
            vec![Step::Pipe {
                text: "a.txt\nb.txt".to_string()
            }]
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn execute_missing_path_fails_before_spawn() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        let err = execute(ShellAction::Open, std::slice::from_ref(&missing)).unwrap_err();
        assert!(
            err.to_string().contains("文件不存在"),
            "错误文案应可展示:{err}"
        );
        let err = execute(ShellAction::RevealInFinder, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
        let err = execute(ShellAction::CopyFileName, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
    }

    #[test]
    fn execute_missing_path_error_is_localized() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        with_lang(Lang::En, || {
            let err = execute(ShellAction::Open, std::slice::from_ref(&missing)).unwrap_err();
            assert!(
                err.to_string()
                    .contains("File not found; the index may be out of date"),
                "错误文案应随语言切换:{err}"
            );
        });
    }

    #[test]
    fn execute_empty_paths_is_noop() {
        execute(ShellAction::Open, &[]).unwrap();
        execute(ShellAction::CopyPaths, &[]).unwrap();
    }
}
