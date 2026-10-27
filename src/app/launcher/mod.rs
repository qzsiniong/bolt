//! Raycast-style launcher: a floating command palette, toggled by a global
//! hotkey.
//!
//! Launcher 是应用的中心 UI，但它对「有哪些模块」一无所知：每次打开时从
//! [`super::shell::AppShell::modules`] 逐个收集 [`LauncherItem`]，按
//! `item.group` 分组渲染；confirm 时把 item 自带的 `on_confirm` 闭包经
//! channel 发回 app tick 执行——confirm 回调里本窗口已经关了，闭包必须
//! 脱离 launcher 实体独立存活（注释见 [`item::LauncherAction`]）。

pub(crate) mod item;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::assets::IconName;
use gpui_kit::base::h_flex;
use gpui_kit::base::input::{InlineToken, InputContent};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::WindowExt;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::prelude::FluentBuilder as _;

use super::usage;
use gpui_kit::*;

use super::shell::AppShell;
use crate::modules::pin::clipboard::read_paste_content;
use crate::ui::clipboard::PasteContent;
pub(crate) use item::{
    DialogSpec, ItemAction, LauncherAction, LauncherIcon, LauncherItem, Module, PasteHandler,
};

gpui_kit::actions!(bolt_launcher, [ToggleActions]);

/// Poll interval for the auto-close-on-blur check.
const BLUR_TICK: Duration = Duration::from_millis(100);

/// 平铺列表（参考 Raycast）：不分段，所有 item 按使用频率全局排序后放单组；
/// `item.group` 只作为行尾的类型徽章显示。`IndexPath { section, row }` 的
/// section 恒为 0，经它解析回 `items` 里的具体一行。
fn flat_groups(items: &[LauncherItem]) -> Vec<(String, Vec<usize>)> {
    if items.is_empty() {
        return Vec::new();
    }
    vec![(String::new(), (0..items.len()).collect())]
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
    // 使用频率排序（宿主公共能力）：全局混排，常用 item 排最前（Raycast 风格
    // 平铺）。所有模块的 item 自动生效，模块自己不用做任何事。
    let usage = usage::load(&usage::usage_path());
    usage::sort_for_launcher(&mut items, &usage);
    let groups = flat_groups(&items);
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
    /// 上一帧真正渲染进 Command 的 item 下标（row → items 下标）。确认回调
    /// 的 IndexPath.row 是这份列表里的位置——粘贴激活时列表被压缩过，不能
    /// 拿 row 去索引全量 items。
    visible_indices: Vec<usize>,
    /// Drained by the app shell, which executes the confirmed action.
    requests: Sender<LauncherAction>,
    /// Cmd+V 识别出的内容；Some 时列表过滤为声明能处理它的 items，回车把
    /// 内容交给选中 item 的 paste handler。与 on_paste hook 闭包共享
    /// （Rc+RefCell；内容包 Arc，确认时 move 进 action 闭包不 clone 图片字节）。
    paste: Rc<RefCell<Option<Arc<PasteContent>>>>,
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
            visible_indices: (0..items.len()).collect(),
            items,
            groups,
            items_revision,
            requests,
            paste: Rc::new(RefCell::new(None)),
            panel: None,
            _blur_task: blur_task,
        }
    }

    fn resolve(&self, section: usize, row: usize) -> Option<&LauncherItem> {
        // 平铺单组：row 直接对应上一帧渲染的可见列表（粘贴激活时已压缩）。
        if section != 0 {
            return None;
        }
        self.visible_indices
            .get(row)
            .and_then(|&index| self.items.get(index))
    }

    fn on_confirm(&mut self, index: IndexPath, window: &mut Window, cx: &mut App) {
        let Some(item) = self.resolve(index.section, index.row) else {
            return;
        };
        // 使用频率：主回车确认即计数（所有模块的 item 通用）。
        usage::record_confirm(cx, &item.id);
        // 粘贴确认：识别内容还在时，把内容交给选中 item 的 paste handler。
        // 同样经 channel 送出——窗口马上要关，闭包必须脱离 launcher 存活。
        if let Some(content) = self.paste.borrow_mut().take()
            && let Some(handler) = &item.paste
        {
            let run = handler.run.clone();
            let _ = self.requests.send(Arc::new(move |cx| run(cx, &content)));
            window.remove_window();
            return;
        }
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

        // 粘贴 token 的生命周期维护：搜索框里的类型 token 被退格删掉后
        // query 变空，此时清掉识别出的内容，恢复完整列表。
        if self.paste.borrow().is_some() && self.state.read(cx).query(cx).trim().is_empty() {
            self.paste.borrow_mut().take();
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
                let _ = this.update(app, |this, cx| this.on_confirm(index, window, cx));
            })
            .on_cancel(|window, _cx| {
                window.remove_window();
            })
            // Cmd+V：识别剪贴板内容（图片/文件/JSON/链接/文本），清空当前
            // 查询并在搜索框里放一个类型 token，列表随之过滤为声明能处理
            // 该内容的 items。识别不出（空剪贴板等）返回 false 走默认粘贴。
            .on_paste({
                let paste = self.paste.clone();
                let state = self.state.clone();
                move |_, window, cx| {
                    let console = cx.global::<AppShell>().console_channel.clone();
                    let Some(content) = read_paste_content(&console) else {
                        return false;
                    };
                    let label = content.kind_label();
                    *paste.borrow_mut() = Some(Arc::new(content));
                    // 原子行内 token：文本流里占住类型名（「图片」等，即
                    // query 的内容），渲染成徽章，退格一次整体删除。
                    let token = InlineToken::new("paste", label).with_label(label);
                    let input = InputContent::new(label)
                        .with_token(0..label.len(), token)
                        .expect("token range exactly covers its own text");
                    state.update(cx, move |state, cx| {
                        state.set_query_content(input, window, cx)
                    });
                    true
                }
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

        // 粘贴激活：只保留声明能处理该内容的 items，并给它们补上类型名
        // 关键词——query 现在就是类型 token 的文本（「图片」等），不补的话
        // Command 自己的文本过滤会把列表清空。
        let paste_filter = self.paste.borrow().clone();
        let paste_label = paste_filter.as_ref().map(|content| content.kind_label());
        let mut visible_indices = Vec::new();
        for (_, indices) in &self.groups {
            // 平铺：单组、无组头（label 是 Option，不设就不渲染分组标题）。
            let mut group = CommandGroup::new();
            for &index in indices {
                let item = &self.items[index];
                if let Some(content) = &paste_filter {
                    let Some(handler) = &item.paste else {
                        continue;
                    };
                    if !(handler.accepts)(content) {
                        continue;
                    }
                }
                visible_indices.push(index);
                // id、subtitle 和额外 keywords（如应用的 original_name / bundle id / URL schemes）
                // 一起喂给 CommandItem。title (label) 本身也参与搜索。
                let mut keywords = vec![item.id.clone()];
                if let Some(subtitle) = &item.subtitle {
                    keywords.push(subtitle.clone());
                }
                keywords.extend(item.keywords.clone());
                if let Some(label) = paste_label {
                    keywords.push(label.to_string());
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
                        // 闭包体内会被 move 的内层闭包引用，先取值（颜色是 Copy）。
                        let muted = cx.theme().muted_foreground;
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
                                this.child(div().text_sm().text_color(muted).child(subtitle))
                            })
                            // 行尾类型徽章（应用/脚本/…）：平铺后没有分组标题，
                            // 靠它区分 item 类型（Raycast 的右侧行内标签）。
                            .child(
                                div()
                                    .ml_auto()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(item_clone.group.clone()),
                            )
                            .into_any_element()
                    })
                    .keywords(keywords);
                group = group.item(entry);
            }
            command = command.group(group);
        }
        // 同步给 confirm 用：回调的 row 是这份压缩后的列表位置。
        self.visible_indices = visible_indices;

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
    use super::LauncherItem;

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
            paste: None,
        }
    }

    #[test]
    fn flat_list_uses_one_section_and_empty_items_yield_none() {
        // 平铺：所有 item 在同一 section，顺序即排序后的顺序。
        let items = vec![item("a", "内置"), item("b", "脚本")];
        let groups = super::flat_groups(&items);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1, [0, 1]);
        assert!(super::flat_groups(&[]).is_empty());
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
