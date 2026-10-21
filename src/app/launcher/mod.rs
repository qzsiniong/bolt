//! Raycast-style launcher: a floating command palette, toggled by a global
//! hotkey.
//!
//! Launcher 是应用的中心 UI，但它对「有哪些模块」一无所知：每次打开时从
//! [`super::shell::AppShell::modules`] 逐个收集 [`LauncherItem`]，按
//! `item.group` 分组渲染；confirm 时把 item 自带的 `on_confirm` 闭包经
//! channel 发回 app tick 执行——confirm 回调里本窗口已经关了，闭包必须
//! 脱离 launcher 实体独立存活（注释见 [`item::LauncherAction`]）。

pub(crate) mod item;

use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::assets::IconName;
use gpui_kit::base::h_flex;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::shell::AppShell;
pub(crate) use item::{LauncherAction, LauncherIcon, LauncherItem, Module};

/// Poll interval for the auto-close-on-blur check.
const BLUR_TICK: Duration = Duration::from_millis(100);

/// 把 items 按 `item.group` 分组：同名组合并，组的位置按首次出现顺序。
/// 返回「组名 → item 下标列表」，`IndexPath { section, row }` 经它解析回
/// `items` 里的具体一行。
fn group_items(items: &[LauncherItem]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match groups.iter_mut().find(|(name, _)| name == &item.group) {
            Some((_, indices)) => indices.push(index),
            None => groups.push((item.group.clone(), vec![index])),
        }
    }
    groups
}

/// 一次收集的结果：items、按组展开的下标，以及收集时的 revision。
struct CollectedItems {
    items: Vec<LauncherItem>,
    groups: Vec<(String, Vec<usize>)>,
    revision: u64,
}

/// 从所有模块收集 items 并分组；同时记下当前的 revision。
///
/// Launcher 打开时收集一次；打开期间若有模块改了内容（例如应用图标生成完
/// 成），模块会把 `AppShell::items_revision` 加一，下一次绘制时这里会被再
/// 调用一次（见 [`Launcher::render`]）。
fn collect_all(cx: &App) -> CollectedItems {
    let mut items = Vec::new();
    for module in &cx.global::<AppShell>().modules {
        items.extend(module.collect_items(cx));
    }
    let groups = group_items(&items);
    let revision = cx.global::<AppShell>().items_revision.get();
    CollectedItems {
        items,
        groups,
        revision,
    }
}

pub struct Launcher {
    state: Entity<CommandState>,
    items: Vec<LauncherItem>,
    /// 组名 → item 下标；与 `items` 同寿命，渲染与 confirm 共用。
    groups: Vec<(String, Vec<usize>)>,
    /// `items` 对应的 `AppShell::items_revision`。
    items_revision: u64,
    /// Drained by the app shell, which executes the confirmed action.
    requests: Sender<LauncherAction>,
    /// Auto-close-on-blur poll; ends when the view is dropped.
    _blur_task: Task<()>,
}

impl Launcher {
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        requests: Sender<LauncherAction>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        // Start with the caret in the search field.
        let focus = state.read(cx).focus_handle(cx);
        focus.focus(window, cx);

        // Re-collect on every open: scripts and extensions may have changed
        // since the last time the launcher was up. Each module reports its
        // own load errors to the console.
        let CollectedItems {
            items,
            groups,
            revision: items_revision,
        } = collect_all(cx);

        let window_handle = window.window_handle();

        // Raycast behavior: once the window has been active, losing activation
        // closes it. Arming only after the first activation avoids closing
        // during the initial focus hand-off.
        let blur_task = cx.spawn(async move |this, cx| {
            let mut was_active = false;
            loop {
                cx.background_executor().timer(BLUR_TICK).await;
                let blurred = this
                    .update(cx, |_, cx| {
                        let active = window_handle
                            .update(cx, |_, window, _| window.is_window_active())
                            .unwrap_or(false);
                        if active {
                            was_active = true;
                        }
                        was_active && !active
                    })
                    .unwrap_or(true);
                if blurred {
                    let _ = this.update(cx, |_, cx| {
                        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
                    });
                    break;
                }
            }
        });

        Self {
            state,
            items,
            groups,
            items_revision,
            requests,
            _blur_task: blur_task,
        }
    }

    fn resolve(&self, section: usize, row: usize) -> Option<&LauncherItem> {
        let index = *self.groups.get(section)?.1.get(row)?;
        self.items.get(index)
    }

    fn on_confirm(&mut self, index: IndexPath, window: &mut Window) {
        let Some(item) = self.resolve(index.section, index.row) else {
            return;
        };
        // If the receiver is gone the app is shutting down: nothing left to do.
        let _ = self.requests.send(item.on_confirm.clone());
        window.remove_window();
    }
}

impl Render for Launcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 模块可能在面板打开期间补齐 items（例如应用图标刚刚生成完）。
        // 只有 revision 变化时才重新收集，正常渲染只读一个 Cell。
        if cx.global::<AppShell>().items_revision.get() != self.items_revision {
            let collected = collect_all(cx);
            self.items = collected.items;
            self.groups = collected.groups;
            self.items_revision = collected.revision;
        }

        // A weak handle: the closure is stored in CommandState (a child of
        // this view), so a strong one would keep both entities alive forever.
        let this = cx.entity().downgrade();
        let mut command = Command::new(&self.state)
            // Fill the window so the list (flex_1) stretches instead of
            // leaving a blank strip below the footer; raise the list's cap
            // accordingly (default 18.75rem is shorter than this window).
            .h_full()
            .max_h(rems(40.))
            .placeholder("Search scripts or commands...")
            .on_confirm(move |index, window, app| {
                let _ = this.update(app, |this, _cx| this.on_confirm(index, window));
            })
            .on_cancel(|window, _cx| {
                window.remove_window();
            })
            .footer(|state, _window, cx| {
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} 个命令", state.matched_count()))
                    .into_any_element()
            });

        for (heading, indices) in &self.groups {
            let mut group = CommandGroup::new().label(heading.clone());
            for &index in indices {
                let item = &self.items[index];
                // The id is searchable too, so `ext.demo.go` finds the command.
                let mut keywords = vec![item.id.clone()];
                if let Some(subtitle) = &item.subtitle {
                    keywords.push(subtitle.clone());
                }
                let item_clone = item.clone();
                let entry = CommandItem::new()
                    .label(item.title.clone())
                    // `child` 会整体替换默认行渲染，这里的 icon 只是占位；
                    // 真实图标在下面的 child 里按 LauncherIcon 分支绘制。
                    .icon(match &item.icon {
                        LauncherIcon::Named(icon) => *icon,
                        LauncherIcon::Image(_) => IconName::AppWindow,
                    })
                    .child(move |_, cx| {
                        h_flex()
                            .flex_1()
                            .gap_2()
                            .items_center()
                            .child(match item_clone.icon.clone() {
                                LauncherIcon::Named(icon) => {
                                    Icon::new(icon).size_4().into_any_element()
                                }
                                // 固定 16×16 容器：图片的行高与内置图标一致，
                                // 列表各行不会因为应用图标尺寸不同而抖动。
                                LauncherIcon::Image(source) => div()
                                    .w(px(16.))
                                    .h(px(16.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(img(source).w(px(16.)).h(px(16.)))
                                    .into_any_element(),
                            })
                            .child(div().child(item_clone.title.clone()))
                            .when_some(item_clone.subtitle.clone(), move |this, subtitle| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                )
                            })
                            .into_any_element()
                    })
                    .keywords(keywords);
                group = group.item(entry);
            }
            command = command.group(group);
        }

        div().size_full().child(command)
    }
}

#[cfg(test)]
mod tests {
    // 显式导入：`super::*` 会把父模块 `gpui_kit::*` 带入的 `#[gpui::test]`
    // 宏一并引进来，令 `#[test]` 递归展开。
    use super::{LauncherItem, group_items};

    fn item(id: &str, group: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: id.to_string(),
            subtitle: None,
            icon: gpui_kit::assets::IconName::Folder.into(),
            group: group.to_string(),
            on_confirm: std::sync::Arc::new(|_| {}),
        }
    }

    #[test]
    fn groups_merge_by_name_and_keep_first_seen_order() {
        let items = vec![
            item("a", "内置"),
            item("b", "脚本"),
            item("c", "内置"),
            item("d", "扩展"),
            item("e", "脚本"),
        ];
        let groups = group_items(&items);
        let names: Vec<&str> = groups.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["内置", "脚本", "扩展"]);
        assert_eq!(groups[0].1, [0, 2]);
        assert_eq!(groups[1].1, [1, 4]);
        assert_eq!(groups[2].1, [3]);
    }

    #[test]
    fn empty_items_yield_no_groups() {
        assert!(group_items(&[]).is_empty());
    }
}
