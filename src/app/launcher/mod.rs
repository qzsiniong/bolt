//! Raycast-style launcher: a floating command palette, toggled by a global
//! hotkey.
//!
//! Launcher 是应用的中心 UI，但它对「有哪些模块」一无所知：每次打开时从
//! [`super::shell::AppShell::modules`] 逐个收集 [`LauncherItem`]，按
//! `item.group` 分组渲染；confirm 时把 item 自带的 `on_confirm` 闭包经
//! channel 发回 app tick 执行——confirm 回调里本窗口已经关了，闭包必须
//! 脱离 launcher 实体独立存活（注释见 [`item::LauncherAction`]）。

pub(crate) mod item;

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::assets::IconName;
use gpui_kit::base::h_flex;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::WindowExt;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::shell::AppShell;
pub(crate) use item::{DialogSpec, ItemAction, LauncherAction, LauncherIcon, LauncherItem, Module};

gpui_kit::actions!(bolt_launcher, [ToggleActions]);

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

/// item 没声明 actions 时回退成单个「打开」（执行 on_confirm）。
/// 无论回退还是声明，都统一追加「显示搜索关键词」——因为 keyword 是
/// Launcher 的核心概念，模块不用自己重复写这条。
fn actions_for(item: &LauncherItem) -> Vec<ItemAction> {
    let mut actions = if item.actions.is_empty() {
        vec![ItemAction {
            title: "打开".to_string(),
            shortcut: Some("↵"),
            run: item.on_confirm.clone(),
            dialog: None,
        }]
    } else {
        item.actions.clone()
    };

    // 搜索命中来源：label（title）、id、subtitle、item.keywords。
    // Command 组件内部对 title/id/subtitle 已经做了匹配，keywords 是额外补
    // 充的；把这几项完整列出来，方便调试和确认匹配来源。
    let item_id = item.id.clone();
    let title = item.title.clone();
    let subtitle = item.subtitle.clone();
    let keywords = item.keywords.clone();
    let mut lines = vec![format!("id: {item_id}"), format!("title: {title}")];
    if let Some(s) = &subtitle {
        lines.push(format!("subtitle: {s}"));
    }
    if keywords.is_empty() {
        lines.push("keywords: (无)".to_string());
    } else {
        lines.push(format!(
            "keywords: [{}]",
            keywords
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // dialog 字段非 None 表示这是展示型动作——Launcher 收到后直接 open dialog，
    // 不走 channel、不关窗口。
    let noop: LauncherAction = Arc::new(|_| {});
    actions.push(ItemAction {
        title: "显示搜索关键词".to_string(),
        shortcut: None,
        run: noop,
        dialog: Some(DialogSpec {
            title: "搜索关键词".to_string(),
            description: lines.join("\n"),
            ok_text: Some("关闭".to_string()),
            show_cancel: false,
        }),
    });
    actions
}

/// Cmd+K 动作面板的打开状态：自己的 CommandState（自带搜索框）+
/// 打开那一刻拍下的动作列表快照。
struct ActionsPanel {
    state: Entity<CommandState>,
    actions: Vec<ItemAction>,
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
    /// Cmd+K 动作面板；None = 关闭。
    panel: Option<ActionsPanel>,
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

        cx.bind_keys([KeyBinding::new("cmd-k", ToggleActions, None)]);

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
            panel: None,
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

    /// Cmd+K：给当前选中项打开/关闭动作面板。
    fn toggle_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.is_some() {
            self.close_actions(window, cx);
            return;
        }
        // 选中项来自主列表的 selection；经 groups 反解回 items。
        let Some(index) = self.state.read(cx).selected_index() else {
            return;
        };
        let Some(item) = self.resolve(index.section, index.row).cloned() else {
            return;
        };
        let state = cx.new(|cx| CommandState::new(window, cx));
        let focus = state.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        self.panel = Some(ActionsPanel {
            state,
            actions: actions_for(&item),
        });
        cx.notify();
    }

    /// 关闭动作面板并把焦点还给主搜索框（不关窗口）。
    fn close_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = None;
        let focus = self.state.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
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
            // 闪电前缀图标 + 大号搜索框（vendor patch 加的 builder 方法）；
            // Cmd+K 动作面板保持默认 Search 图标。
            .search_icon(IconName::Zap)
            .search_size(gpui_kit::component::Size::Large)
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
                // id、subtitle 和额外 keywords（如应用的 original_name / bundle id / URL schemes）
                // 一起喂给 CommandItem。title (label) 本身也参与搜索。
                let mut keywords = vec![item.id.clone()];
                if let Some(subtitle) = &item.subtitle {
                    keywords.push(subtitle.clone());
                }
                keywords.extend(item.keywords.clone());
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
                                    Icon::new(icon).size_6().into_any_element()
                                }
                                // 固定 24×24 容器：图片的行高与内置图标一致，
                                // 列表各行不会因为应用图标尺寸不同而抖动。
                                LauncherIcon::Image(source) => div()
                                    .w(px(24.))
                                    .h(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(img(source).w(px(24.)).h(px(24.)))
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

        let mut root = div()
            .size_full()
            .relative()
            .on_action(cx.listener(|this, _: &ToggleActions, window, cx| {
                this.toggle_actions(window, cx);
            }))
            .child(command);

        // Cmd+K 动作面板：右下角浮层，自带搜索框的第二个 Command。
        // Esc 只关面板（on_cancel 里 close_actions），Enter 执行选中动作。
        if let Some(panel) = &self.panel {
            let actions = panel.actions.clone();
            let requests = self.requests.clone();
            let this = cx.entity().downgrade();
            let mut panel_command = Command::new(&panel.state)
                .max_h(rems(22.))
                // 搜索框放底部，对齐 Raycast 动作面板。
                .search_bottom(true)
                .placeholder("搜索操作…")
                .on_confirm(move |index, window, _app| {
                    let Some(action) = actions.get(index.row) else {
                        return;
                    };
                    // 展示型动作：直接弹 AlertDialog，不关窗口（面板保留）。
                    // 执行型动作：经 channel 发 LauncherAction，然后关窗口。
                    if let Some(spec) = &action.dialog {
                        let spec = spec.clone();
                        window.open_alert_dialog(_app, move |alert, _, _| {
                            let mut dlg = alert
                                .title(spec.title.clone())
                                .description(spec.description.clone())
                                .show_cancel(spec.show_cancel);
                            if let Some(ok) = &spec.ok_text {
                                dlg = dlg.ok_text(ok.clone());
                            }
                            dlg.on_ok(move |_, _, _| true)
                        });
                    } else {
                        let _ = requests.send(action.run.clone());
                        window.remove_window();
                    }
                })
                .on_cancel(move |window, app| {
                    let _ = this.update(app, |this, cx| this.close_actions(window, cx));
                });

            let mut group = CommandGroup::new();
            for action in &panel.actions {
                let title = action.title.clone();
                let shortcut = action.shortcut;
                group = group.item(
                    CommandItem::new()
                        .label(action.title.clone())
                        .keywords(vec![action.title.clone()])
                        .child(move |_, cx| {
                            h_flex()
                                .flex_1()
                                .items_center()
                                .justify_between()
                                .child(div().child(title.clone()))
                                .when_some(shortcut, |this, s| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(s),
                                    )
                                })
                                .into_any_element()
                        }),
                );
            }
            panel_command = panel_command.group(group);

            root = root.child(
                div()
                    .absolute()
                    .bottom_4()
                    .right_4()
                    // 注册遮挡 hitbox，阻止 hover/滚动穿透到底层 command 列表。
                    .occlude()
                    .w(px(420.))
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .shadow_lg()
                    .child(panel_command),
            );
        }

        root
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
            keywords: Vec::new(),
            on_confirm: std::sync::Arc::new(|_| {}),
            actions: Vec::new(),
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

    #[test]
    fn actions_fallback_has_open_plus_show_keywords() {
        let it = item("a", "内置");
        let actions = super::actions_for(&it);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].title, "打开");
        assert_eq!(actions[0].shortcut, Some("↵"));
        assert_eq!(actions[1].title, "显示搜索关键词");
    }

    #[test]
    fn declared_actions_are_preserved_then_show_keywords_appended() {
        let mut it = item("a", "内置");
        it.actions = vec![
            super::ItemAction {
                title: "第一个".to_string(),
                shortcut: None,
                run: std::sync::Arc::new(|_| {}),
                dialog: None,
            },
            super::ItemAction {
                title: "第二个".to_string(),
                shortcut: None,
                run: std::sync::Arc::new(|_| {}),
                dialog: None,
            },
        ];
        let actions = super::actions_for(&it);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].title, "第一个");
        assert_eq!(actions[1].title, "第二个");
        assert_eq!(actions[2].title, "显示搜索关键词");
    }
}
