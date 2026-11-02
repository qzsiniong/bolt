//! 扩展的管理操作：启用 / 禁用、重载、卸载、定位目录。
//!
//! 所有入口（Launcher 的 Cmd+K、管理窗口的按钮）都走这里，行为只有一份。
//! 每个函数的形状都一样：拿 `ActionSubject`（纯快照）→ 改 `ExtensionsHost`
//! → 把结果写 Console → `items_changed` 让 Launcher 重新收集。

use gpui_kit::App;

use super::actions::ActionSubject;
use super::manifest::{ensure_extension_data_dir, extension_data_dir};
use super::state::ExtensionsState;
use super::{report_to_console, ui_window};
use crate::app::shell::AppShell;
use crate::app::windows;
use crate::platform;
use crate::ui::notify_window;

/// 启用 / 禁用。禁用 = 不注册、不加载，Launcher 里命令直接消失。
pub(crate) fn set_enabled(cx: &mut App, subject: &ActionSubject, enabled: bool) {
    let outcome = {
        let state = cx.global::<ExtensionsState>();
        let mut host = state.host.borrow_mut();
        host.set_enabled(&subject.extension_id, enabled)
    };
    match outcome {
        Ok(report) => {
            report_to_console(cx, &report);
            console(cx).info(format!(
                "扩展 {} 已{}",
                subject.label(),
                if enabled {
                    "启用"
                } else {
                    "禁用（不再加载）"
                }
            ));
            windows::items_changed(cx);
        }
        Err(error) => console(cx).warn(format!(
            "{}扩展 {} 失败：{error}",
            if enabled { "启用" } else { "禁用" },
            subject.label()
        )),
    }
}

/// 重载：重读清单 + 重建运行时。先关掉该扩展的 UI 窗口——它们指向旧的
/// `ShellRoot`，注册表换了之后继续留着只会一路报错。
pub(crate) fn reload(cx: &mut App, subject: &ActionSubject) {
    let closed = ui_window::close_extension_windows(cx, &subject.root);
    let outcome = {
        let state = cx.global::<ExtensionsState>();
        let mut host = state.host.borrow_mut();
        host.reload(&subject.extension_id)
    };
    match outcome {
        Ok(report) => {
            report_to_console(cx, &report);
            console(cx).info(format!(
                "扩展 {} 已重新加载{}",
                subject.label(),
                if closed > 0 {
                    format!("（关闭了 {closed} 个窗口，重新打开命令即用新代码）")
                } else {
                    String::new()
                }
            ));
            windows::items_changed(cx);
        }
        Err(error) => console(cx).warn(format!("重载扩展 {} 失败：{error}", subject.label())),
    }
}

/// 在 Finder 中显示扩展目录（选中那一层）。
///
/// 拉起 Finder 的动作**放到工作线程**（`platform::reveal_in_background` 内部起
/// 线程）：调用方是 GPUI 的点击处理器，在里面同步激活别的 App 会让 AppKit 在
/// 自己的调用栈里就把失焦回调派发回来，GPUI 随即再借一次 App → 直接 abort。
pub(crate) fn reveal(cx: &App, subject: &ActionSubject) {
    let root = subject.root.clone();
    let console = console(cx).clone();
    platform::reveal_in_background(root.clone(), move |result| {
        if let Err(error) = result {
            console.warn(format!("无法在 Finder 中显示 {}：{error}", root.display()));
        }
    });
}

/// 打开扩展的私有数据目录（`extensions-data/<id>/`）。建目录留在主线程（快），
/// 打开窗口交给工作线程，原因同 [`reveal`]。
pub(crate) fn open_data_dir(cx: &App, subject: &ActionSubject) {
    let dir = match ensure_extension_data_dir(&subject.extension_id) {
        Ok(dir) => dir,
        Err(error) => {
            console(cx).warn(format!(
                "扩展 {} 的数据目录不可用：{error}",
                subject.label()
            ));
            extension_data_dir(&subject.extension_id)
        }
    };
    let console = console(cx).clone();
    platform::open_in_background(dir.clone(), move |result| {
        if let Err(error) = result {
            console.warn(format!("无法打开 {}：{error}", dir.display()));
        }
    });
}

/// 卸载：注销 → 移进废纸篓 → 重新扫描。
///
/// 顺序就是正确性：注销先于删目录（运行时先收掉），磁盘一个字节都不动，
/// 注销失败就不会走到 trash 那步。
pub(crate) fn uninstall(cx: &mut App, subject: &ActionSubject) {
    let closed = ui_window::close_extension_windows(cx, &subject.root);

    if let Err(error) = {
        let state = cx.global::<ExtensionsState>();
        let mut host = state.host.borrow_mut();
        host.forget(&subject.extension_id)
    } {
        console(cx).warn(format!("卸载扩展 {} 失败：{error}", subject.label()));
        return;
    }

    let moved = match platform::trash_item(&subject.root) {
        Ok(()) => Ok(format!("已移到废纸篓：{}", subject.root.display())),
        Err(error) => {
            // 系统废纸篓失败（权限、远程卷）时退回纯 fs 重命名；平台没有可写的
            // 废纸篓目录（Windows 的回收站不是普通目录）或回退也失败，就**不删**，
            // 让用户自己处理——删用户文件不是"尽力而为"的操作。
            let fallback = platform::trash_dir()
                .ok_or_else(|| error.clone())
                .and_then(|trash| platform::move_to_trash(&subject.root, &trash));
            match fallback {
                Ok(target) => Ok(format!(
                    "系统废纸篓不可用（{error}），已移到 {}",
                    target.display()
                )),
                Err(fallback_error) => Err(format!(
                    "{error}；回退也失败（{fallback_error}）。目录仍在原处，请手动删除"
                )),
            }
        }
    };

    // 无论磁盘那步成没成，注册表都按现状重新收敛：目录还在就会被重新注册。
    {
        let state = cx.global::<ExtensionsState>();
        let mut host = state.host.borrow_mut();
        let report = host.refresh();
        drop(host);
        report_to_console(cx, &report);
    }

    match moved {
        Ok(message) => {
            console(cx).info(format!("卸载扩展 {}：{message}", subject.label()));
            notify_window::notify(
                "Bolt",
                &format!(
                    "已卸载扩展 {}{}",
                    subject.label(),
                    if closed > 0 {
                        format!("（关闭了 {closed} 个窗口）")
                    } else {
                        String::new()
                    }
                ),
            );
        }
        Err(error) => console(cx).error(format!("卸载扩展 {} 失败：{error}", subject.label())),
    }
    windows::items_changed(cx);
}

fn console(cx: &App) -> &bolt_console::ConsoleChannel {
    &cx.global::<AppShell>().console_channel
}

/// 供 `actions::extension_actions` 构造卸载确认文案。
pub(crate) fn uninstall_description(subject: &ActionSubject) -> String {
    let mut text = format!(
        "扩展 {}\n{}\n\n目录会被移到废纸篓，可以再拖回来恢复。",
        subject.label(),
        subject.root.display()
    );
    if super::manifest::uses_bundled_extensions() {
        text.push_str("\n\n开发构建：这是仓库里的扩展目录，移走后需要用 git 恢复。");
    }
    text
}
