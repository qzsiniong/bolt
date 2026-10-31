//! Raycast 风格的启动器：一个浮动命令面板，由全局快捷键唤起/关闭。
//!
//! Launcher 是应用的中心 UI，但它对「有哪些模块」一无所知：每次打开时从
//! [`super::shell::AppShell::modules`] 逐个收集 [`LauncherItem`]，平铺渲染；
//! 确认时把 item 自带的 `on_confirm` 闭包经 channel 发回 app tick 执行——
//! confirm 回调里本窗口已经关了，闭包必须脱离 launcher 实体独立存活
//! （注释见 [`item::LauncherAction`]）。
//!
//! 搜索框由宿主自定义：`Command::searchable(false)` 关掉 gpui-component 内置
//! 搜索框与文本过滤，`Input` 放在 Command 外面。这样 Cmd+V 的剪贴板识别、
//! 类型 token 的图标渲染和「token 后继续输入文字二次过滤」都不需要给依赖
//! 打 patch；列表的过滤（文本 + 粘贴类型）统一在构建 model 时做。键盘上，
//! Enter/Escape 从聚焦的 Input 以 action 形式冒泡到根（单行 Input 的
//! enter/escape handler 会 propagate），↑↓ 的 MoveUp/MoveDown 对单行是
//! no-op 且不阻止传播，keydown listener 收得到——三者在 root 统一分流。

pub(crate) mod actions;
pub(crate) mod collect;
pub(crate) mod filter;
pub(crate) mod item;
pub(crate) mod list;
pub(crate) mod panel;

pub(crate) use item::{
    DialogSpec, ItemAction, LauncherAction, LauncherIcon, LauncherItem, Module, PasteHandler,
};

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::assets::IconName;
use gpui_kit::base::input::{InlineToken, InlineTokenContext, InputContent};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Enter, Escape, Input, InputEvent, InputState, InputToken};
use gpui_kit::prelude::FluentBuilder as _;

use super::shell::AppShell;
use super::usage;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, Focusable, InteractiveElement, IntoElement,
    KeyBinding, KeyDownEvent, ParentElement, Render, ScrollHandle, Styled, Task, Window, div, img,
    px,
};

use crate::ui::clipboard::{PendingPaste, peek_paste, read_paste_content};

use actions::{actions_for, open_action_dialog};
use collect::{CollectedItems, collect_all};
use filter::{strip_token_text, visible_indices};
use panel::{ActionsPanel, render_overlay};

gpui_kit::actions!(bolt_launcher, [ToggleActions]);

/// 失焦自动关窗检查的轮询间隔。
const BLUR_TICK: Duration = Duration::from_millis(100);

/// 输入框的「有效查询」：剥掉类型 token 的占位文本，剩用户手输的部分。
/// 粘贴 token 后继续输入文字，过滤就只看这部分。
fn effective_query(input: &InputState) -> String {
    strip_token_text(input.value().as_ref(), input.tokens())
}

/// Launcher 主面板：搜索框 + 自绘命令列表 + Cmd+K 动作面板。
pub struct Launcher {
    /// 主搜索框（宿主自有）：Cmd+V 识别、token 渲染、Change 订阅都挂它。
    input: Entity<InputState>,
    /// 有效查询（剥 token 后的手输文本）；随 input 的 Change 同步。
    query_filter: String,
    items: Vec<LauncherItem>,
    /// `items` 对应的 `AppShell::items_revision`。
    items_revision: u64,
    /// 过滤条件（query_filter / paste）变更计数；render 依它把选中重置到
    /// 首项。
    filter_epoch: u64,
    /// render 已应用过的 `filter_epoch`。
    applied_filter_epoch: u64,
    /// 上一帧渲染进列表的 item 下标（row → items 下标）。确认回调的 row 是
    /// 这份列表里的位置——粘贴/文本过滤后列表被压缩过，不能拿 row 去索引
    /// 全量 items。
    visible_indices: Vec<usize>,
    /// 主列表选中行（`visible_indices` 里的下标）。None = 空列表。
    ///
    /// 只有单击、↑/↓ 与「过滤后重置到首项」会改它——hover 不动选中。
    selected: Option<usize>,
    /// 列表滚动句柄：选中行变了就把它带进视野。
    scroll: ScrollHandle,
    /// Cmd+V 识别出的粘贴（图片惰性：只识别类型，字节确认时才读）。Some 时
    /// 列表过滤为声明能处理该类型的 items，回车把内容交给选中 item 的
    /// paste handler。与 on_paste hook 闭包共享（Rc+RefCell）。
    paste: Rc<RefCell<Option<PendingPaste>>>,
    /// 由 app shell 排空并执行确认后的动作。
    requests: Sender<LauncherAction>,
    /// Cmd+K 动作面板；None = 关闭。
    panel: Option<ActionsPanel>,
    /// 失焦自动关窗的轮询任务；视图 drop 时结束。
    _blur_task: Task<()>,
}

impl Launcher {
    /// 创建 Launcher：收集全部模块 items、聚焦搜索框并启动失焦自动关窗轮询。
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        requests: Sender<LauncherAction>,
    ) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search scripts or commands..."));
        // 主输入框变化时重算过滤（用户打字 / 退格删 token）。
        cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.sync_filter(cx)
            }
        })
        .detach();
        // 让光标一开始就落在搜索框里。
        let focus = input.read(cx).focus_handle(cx);
        focus.focus(window, cx);

        cx.bind_keys([KeyBinding::new("cmd-k", ToggleActions, None)]);

        // 每次打开都重新收集：脚本和扩展自上次唤起后可能变了。各模块的
        // 加载错误自己报到 console。
        let CollectedItems {
            items,
            revision: items_revision,
        } = collect_all(cx);
        let item_count = items.len();

        let window_handle = window.window_handle();

        // Raycast 行为：窗口一旦激活过，失去激活就关窗。首次激活之后才
        // 启用（arm），避免初始焦点交接期间被误关。
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
            input,
            query_filter: String::new(),
            items,
            items_revision,
            filter_epoch: 0,
            applied_filter_epoch: 0,
            visible_indices: (0..item_count).collect(),
            selected: (item_count > 0).then_some(0),
            scroll: ScrollHandle::new(),
            paste: Rc::new(RefCell::new(None)),
            requests,
            panel: None,
            _blur_task: blur_task,
        }
    }

    /// 可见列表里的第 `row` 行对应的 item。
    fn resolve(&self, row: usize) -> Option<&LauncherItem> {
        self.visible_indices
            .get(row)
            .and_then(|&index| self.items.get(index))
    }

    /// 主回车确认：读当前选中行走 [`Self::on_confirm`]（空列表无选中则忽略）。
    fn confirm_main(&mut self, window: &mut Window, cx: &mut App) {
        let Some(row) = self.selected else {
            return;
        };
        self.on_confirm(row, window, cx);
    }

    fn on_confirm(&mut self, row: usize, window: &mut Window, cx: &mut App) {
        let Some(item) = self.resolve(row) else {
            return;
        };
        // 使用频率：主回车确认即计数（所有模块的 item 通用）。
        usage::record_confirm(cx, &item.id);
        // 粘贴确认：识别内容还在时，把内容交给选中 item 的 paste handler。
        // 图片在这里才真正读剪贴板 + 转 PNG——转码慢，挪到后台线程：回车
        // 立即关窗（体感与普通确认一致），内容读好后 handler 才执行，pin
        // 窗口晚几百毫秒出现。动作闭包都脱离 launcher 独立存活。
        if let Some(pending) = self.paste.borrow_mut().take()
            && let Some(handler) = &item.paste
        {
            let run = handler.run.clone();
            match pending {
                PendingPaste::Ready(content) => {
                    let content = Arc::new(content);
                    let _ = self.requests.send(Arc::new(move |cx| run(cx, &content)));
                }
                PendingPaste::Image(_) => {
                    // 剪贴板在后台读取完成前被清（读不出内容）时静默放弃：
                    // 窗口已关，回退普通确认只会执行错动作。
                    let console = cx.global::<AppShell>().console_channel.clone();
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(read_paste_content(&console));
                    });
                    cx.spawn(async move |cx| {
                        if let Ok(Some(content)) = rx.await {
                            let content = Arc::new(content);
                            cx.update(|cx| run(cx, &content));
                        }
                    })
                    .detach();
                }
            }
            window.remove_window();
            return;
        }
        // 接收端已不在说明应用正在退出：无事可做。
        let _ = self.requests.send(item.on_confirm.clone());
        window.remove_window();
    }

    /// 输入变化后重算有效查询，并标记过滤条件变更（render 会重置选中）。
    fn sync_filter(&mut self, cx: &mut Context<Self>) {
        let query = effective_query(self.input.read(cx));
        if query == self.query_filter {
            return;
        }
        self.query_filter = query;
        self.filter_epoch += 1;
        cx.notify();
    }

    /// Cmd+V：只识别剪贴板内容的类型（图片不读字节，见 [`PendingPaste`]），
    /// 清空当前查询并在搜索框里放一个带图标的类型 token，列表随之过滤为
    /// 声明能处理该内容的 items。识别不出（空剪贴板等）返回 false 走默认粘贴。
    fn handle_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let console = cx.global::<AppShell>().console_channel.clone();
        let Some(pending) = peek_paste(&console) else {
            return false;
        };
        // token 显示内容预览（文件名 / 前 n 个字符），图标照旧按类型映射。
        let text = pending.token_text();
        let icon_key = pending.icon_key();
        self.paste.borrow_mut().replace(pending);
        // 原子行内 token：文本流里占住预览文本，渲染成「图标 + 预览」徽章，
        // 退格一次整体删除；id 存图标 key，渲染端按它映射 Lucide 图标。
        let token = InlineToken::new(icon_key, text.as_str()).with_label(text.as_str());
        let content_input = InputContent::new(text.as_str())
            .with_token(0..text.len(), token)
            .expect("token range exactly covers its own text");
        self.input
            .update(cx, |input, cx| input.set_value(content_input, window, cx));
        // set_value 不发 Change 事件：手动同步过滤与选中。
        self.query_filter = String::new();
        self.filter_epoch += 1;
        cx.notify();
        true
    }

    /// ↑/↓：在可见序列里移动选中，并把新选中行滚进视野。
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let visible_len = self.visible_indices.len();
        if visible_len == 0 {
            return;
        }
        let current = self.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, visible_len as isize - 1) as usize;
        self.select_row(next, cx);
    }

    /// 把选中设到第 `row` 行（单击、↑/↓、过滤后重置都走这里）。
    fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = Some(row);
        self.scroll.scroll_to_item(row);
        cx.notify();
    }

    /// Cmd+K：给当前选中项打开/关闭动作面板。
    fn toggle_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.is_some() {
            self.close_actions(window, cx);
            return;
        }
        // 选中项来自主列表的 selection；经 visible_indices 反解回 items。
        let Some(row) = self.selected else {
            return;
        };
        let Some(item) = self.resolve(row).cloned() else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("搜索操作…"));
        cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
            if let InputEvent::Change = event
                && let Some(panel) = this.panel.as_mut()
            {
                let query = effective_query(panel.input.read(cx));
                if query != panel.query_filter {
                    panel.query_filter = query;
                    panel.filter_epoch += 1;
                    cx.notify();
                }
            }
        })
        .detach();
        // 焦点交给面板搜索框。
        let focus = input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        let actions = actions_for(&item);
        self.panel = Some(ActionsPanel {
            input,
            query_filter: String::new(),
            filter_epoch: 0,
            applied_filter_epoch: 0,
            visible: (0..actions.len()).collect(),
            selected: (!actions.is_empty()).then_some(0),
            scroll: ScrollHandle::new(),
            actions,
        });
        cx.notify();
    }

    /// 关闭动作面板并把焦点还给主搜索框（不关窗口）。
    fn close_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = None;
        let focus = self.input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    /// 面板确认：执行可见列表里当前选中的动作。
    fn confirm_action_panel(&mut self, window: &mut Window, cx: &mut App) {
        let (actions, visible, selected) = {
            let Some(panel) = self.panel.as_ref() else {
                return;
            };
            (panel.actions.clone(), panel.visible.clone(), panel.selected)
        };
        let Some(row) = selected else {
            return;
        };
        let Some(&index) = visible.get(row) else {
            return;
        };
        let Some(action) = actions.get(index) else {
            return;
        };
        // 展示/编辑型动作：直接弹 AlertDialog，不关窗口（面板保留）。
        // 执行型动作：经 channel 发 LauncherAction，然后关窗口。
        if let Some(spec) = &action.dialog {
            open_action_dialog(window, cx, spec);
        } else {
            let _ = self.requests.send(action.run.clone());
            window.remove_window();
        }
    }

    /// 面板 ↑/↓ 移动选中。
    fn move_panel_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let visible_len = panel.visible.len();
        if visible_len == 0 {
            return;
        }
        let current = panel.selected.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, visible_len as isize - 1) as usize;
        panel.selected = Some(next);
        panel.scroll.scroll_to_item(next);
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
            self.items_revision = collected.revision;
        }

        // 粘贴 token 的生命周期维护：搜索框里的类型 token 被退格删掉后
        // 输入框不再有 token，此时清掉识别出的内容，恢复完整列表。
        let has_token = !self.input.read(cx).tokens().is_empty();
        if self.paste.borrow().is_some() && !has_token {
            self.paste.borrow_mut().take();
            self.filter_epoch += 1;
        }

        // 过滤条件变更后把选中重置到首项（并把列表滚回顶部）。
        if self.applied_filter_epoch != self.filter_epoch {
            self.applied_filter_epoch = self.filter_epoch;
            let empty = {
                let paste = self.paste.borrow();
                visible_indices(&self.items, &self.query_filter, paste.as_ref()).is_empty()
            };
            self.selected = (!empty).then_some(0);
            self.scroll.scroll_to_item(0);
        }

        let visible = {
            // 只在借用窗口内过滤（PendingPaste::Ready 里带字节，不能 clone）。
            let paste = self.paste.borrow();
            visible_indices(&self.items, &self.query_filter, paste.as_ref())
        };
        let visible_count = visible.len();
        self.visible_indices = visible;
        // items 可能在面板打开期间变化（图标补齐、扩展重载）：列表变短时把
        // 选中夹回范围内，别指向不存在的一行。
        if self
            .selected
            .is_some_and(|row| row >= self.visible_indices.len())
        {
            self.selected = self.visible_indices.len().checked_sub(1);
        }

        // 自绘列表：hover 只淡高亮、单击选中、双击执行（见 `list` 模块）。
        let mut rows: Vec<AnyElement> = Vec::with_capacity(self.visible_indices.len());
        for (row, &index) in self.visible_indices.iter().enumerate() {
            let item = &self.items[index];
            let content = row_content(item, cx);
            let launcher = cx.entity().downgrade();
            rows.push(list::row(
                row,
                self.selected == Some(row),
                content,
                cx,
                move |event, window, cx| {
                    if list::is_activation(event) {
                        // 确认会关窗口：延到本轮事件派发结束后再执行，别在行自己
                        // 的 click 处理里拆窗口（旧 Command 的 confirm 也是 defer）。
                        let launcher = launcher.clone();
                        window.defer(cx, move |window, cx| {
                            launcher
                                .update(cx, |launcher, cx| launcher.on_confirm(row, window, cx))
                                .ok();
                        });
                    } else {
                        launcher
                            .update(cx, |launcher, cx| {
                                launcher.select_row(row, cx);
                                // 点一下不该把键盘焦点从搜索框带走：继续输入即可。
                                let focus = launcher.input.read(cx).focus_handle(cx);
                                focus.focus(window, cx);
                            })
                            .ok();
                    }
                },
            ));
        }
        let list = list::container("launcher-list", &self.scroll, None, rows);

        let footer = div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{visible_count} 个命令"));

        // 主搜索框：大号 + 闪电前缀（对齐原 Command 内置搜索框样式）。
        let search_input = Input::new(&self.input)
            .large()
            .appearance(false)
            .prefix(Icon::new(IconName::Zap).text_color(cx.theme().muted_foreground))
            // 类型 token 渲染：id 存的是 icon_key，按它映射 Lucide 图标。
            .token(move |ctx: &InlineTokenContext, _window, cx| {
                let muted = cx.theme().muted_foreground;
                let icon = match ctx.token().id().as_ref() {
                    "image" => Some(IconName::Image),
                    "file" => Some(IconName::FileText),
                    "json" => Some(IconName::Braces),
                    "link" => Some(IconName::Link),
                    "text" => Some(IconName::Type),
                    _ => None,
                };
                let mut token = InputToken::new(ctx);
                if let Some(icon) = icon {
                    token = token.icon(Icon::new(icon).text_color(muted));
                }
                token.into_any_element()
            })
            .on_paste({
                let launcher = cx.entity();
                move |_, window, cx| launcher.update(cx, |this, cx| this.handle_paste(window, cx))
            });

        let mut root = div()
            .size_full()
            .relative()
            .on_action(cx.listener(|this, _: &Enter, window, cx| {
                if this.panel.is_some() {
                    this.confirm_action_panel(window, cx);
                } else {
                    this.confirm_main(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.panel.is_some() {
                    this.close_actions(window, cx);
                } else {
                    window.remove_window();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(key, "up" | "down") || event.keystroke.modifiers.modified() {
                    return;
                }
                cx.stop_propagation();
                let delta = if key == "up" { -1 } else { 1 };
                if this.panel.is_some() {
                    this.move_panel_selection(delta, cx);
                } else {
                    this.move_selection(delta, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleActions, window, cx| {
                this.toggle_actions(window, cx);
            }))
            .child(
                v_flex()
                    .size_full()
                    // 窗口本身没有表面色：原来由 Command 画，现在挂在根容器上。
                    .bg(cx.theme().popover)
                    .text_color(cx.theme().popover_foreground)
                    .child(div().px_3().pt_3().child(search_input))
                    .child(v_flex().flex_1().min_h_0().child(list).child(footer)),
            );

        // Cmd+K 动作面板：右下角浮层，列表在上、搜索框在底部（对齐 Raycast）。
        if let Some(panel) = &mut self.panel {
            let overlay = render_overlay(panel, &self.requests, cx.entity().downgrade(), cx);
            root = root.child(overlay);
        }

        root
    }
}

/// 一行的内容：图标 + 标题 + 副标题 + 行尾类型徽章。
///
/// 行的选中/hover 样式与点击语义在 [`list::row`] 里，这里只管内容。
fn row_content(item: &LauncherItem, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .flex_1()
        .gap_2()
        .items_center()
        .child(match item.icon.clone() {
            LauncherIcon::Named(icon) => Icon::new(icon).size_8().into_any_element(),
            // 扩展清单声明的图标路径：扩展自己的 icons/ 优先，内置 Lucide 兜底。
            LauncherIcon::Asset(path) => Icon::new(crate::app::assets::AssetIcon(path))
                .size_8()
                .into_any_element(),
            // 固定 32×32 容器：图片的行高与内置图标一致，列表各行不会因为
            // 应用图标尺寸不同而抖动。
            LauncherIcon::Image(source) => div()
                .w(px(32.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .child(img(source).w(px(32.)).h(px(32.)))
                .into_any_element(),
        })
        .child(div().child(item.title.clone()))
        .when_some(item.subtitle.clone(), move |this, subtitle| {
            this.child(div().text_sm().text_color(muted).child(subtitle))
        })
        // 行尾类型徽章（应用/脚本/…）：平铺后没有分组标题，靠它区分类型
        // （Raycast 的右侧行内标签）。
        .child(
            div()
                .ml_auto()
                .text_xs()
                .text_color(muted)
                .child(item.group.clone()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // 显式导入：`super::*` 会把父模块 `gpui_kit::*` 带入的 `#[gpui::test]`
    // 宏一并引进来，令 `#[test]` 递归展开。
    use crate::app::keywords;

    #[test]
    fn parse_keywords_splits_and_trims() {
        assert_eq!(
            keywords::parse("微信, wechat，pin"),
            vec!["微信".to_string(), "wechat".to_string(), "pin".to_string()]
        );
        assert!(keywords::parse("  , ， ").is_empty());
    }
}
