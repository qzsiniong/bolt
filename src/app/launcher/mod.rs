//! Raycast-style launcher: a floating command palette, toggled by a global
//! hotkey.
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

pub(crate) mod item;

pub(crate) use item::{
    DialogSpec, ItemAction, LauncherAction, LauncherIcon, LauncherItem, Module, PasteHandler,
};

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use gpui_kit::assets::IconName;
use gpui_kit::base::input::{InlineToken, InlineTokenContext, InlineTokenSpan, InputContent};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::WindowExt;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Enter, Escape, Input, InputEvent, InputState, InputToken};
use gpui_kit::prelude::FluentBuilder as _;

use super::usage;
use gpui_kit::*;

use super::shell::AppShell;
use crate::modules::pin::clipboard::{peek_paste, read_paste_content};
use crate::ui::clipboard::{PasteContent, PendingPaste};

gpui_kit::actions!(bolt_launcher, [ToggleActions]);

/// Poll interval for the auto-close-on-blur check.
const BLUR_TICK: Duration = Duration::from_millis(100);

/// 一次收集的结果：items 与收集时的 revision。
struct CollectedItems {
    items: Vec<LauncherItem>,
    revision: u64,
}

/// 从所有模块收集 items；同时记下当前的 revision。
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
    // 用户编辑过的搜索关键词（item-keywords.json）覆盖模块默认值。
    usage::apply_keyword_overrides(&mut items, &usage::keywords_path());
    let revision = cx.global::<AppShell>().items_revision.get();
    CollectedItems { items, revision }
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

    // 搜索命中来源：label（title）、id、subtitle、item.keywords。过滤在
    // visible_indices 里做（title/id/subtitle/keywords 都参与）；把这几项
    // 列出来供「显示搜索关键词」调试动作展示。
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
    // dialog 字段非 None 表示这是展示/编辑型动作——Launcher 收到后直接在
    // 当前窗口 open dialog，不走 channel、不关窗口。
    let noop: LauncherAction = Arc::new(|_| {});
    actions.push(ItemAction {
        title: "显示搜索关键词".to_string(),
        shortcut: None,
        run: noop,
        dialog: Some(DialogSpec::Info {
            title: "搜索关键词".to_string(),
            description: lines.join("\n"),
            ok_text: Some("关闭".to_string()),
            show_cancel: false,
        }),
    });
    // 编辑动作：覆盖表按 item id 存盘（空输入恢复模块默认），保存后
    // items_revision++ 让下一次 collect 拿到新关键词。
    let edited_id = item.id.clone();
    let noop: LauncherAction = Arc::new(|_| {});
    actions.push(ItemAction {
        title: "编辑搜索关键词".to_string(),
        shortcut: None,
        run: noop,
        dialog: Some(DialogSpec::Edit {
            title: format!("编辑搜索关键词 — {title}"),
            initial: keywords.join(", "),
            ok_text: Some("保存".to_string()),
            on_submit: Arc::new(move |cx, text| usage::set_keywords(cx, &edited_id, &text)),
        }),
    });
    actions
}

/// 在当前窗口打开动作的 dialog：展示型（Info）直接展示；编辑型（Edit）带
/// 输入框，OK 时把输入内容交给 `on_submit`。输入框 Entity 在闭包外创建——
/// `open_alert_dialog` 的 build 闭包与 content builder 每帧都会被调用，
/// 不能在里面 `cx.new`。
fn open_action_dialog(window: &mut Window, cx: &mut App, spec: &DialogSpec) {
    match spec {
        DialogSpec::Info {
            title,
            description,
            ok_text,
            show_cancel,
        } => {
            let (title, description, ok_text, show_cancel) = (
                title.clone(),
                description.clone(),
                ok_text.clone(),
                *show_cancel,
            );
            window.open_alert_dialog(cx, move |alert, _, _| {
                let mut dlg = alert
                    .title(title.clone())
                    .description(description.clone())
                    .show_cancel(show_cancel);
                if let Some(ok) = &ok_text {
                    dlg = dlg.ok_text(ok.clone());
                }
                dlg.on_ok(move |_, _, _| true)
            });
        }
        DialogSpec::Edit {
            title,
            initial,
            ok_text,
            on_submit,
        } => {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("逗号分隔，例如：微信, wechat（留空恢复默认）")
            });
            input.update(cx, |state, cx| state.set_value(initial.clone(), window, cx));
            let (title, ok_text, on_submit) = (title.clone(), ok_text.clone(), on_submit.clone());
            window.open_alert_dialog(cx, move |alert, _, _| {
                let mut dlg = alert
                    .title(title.clone())
                    // 内层闭包 own 一份 clone（外层是 Fn，构造内层 move 闭包
                    // 时只能动局部值，不能 move 捕获）。
                    .content({
                        let input = input.clone();
                        move |content, _, _| content.child(Input::new(&input))
                    })
                    .on_ok({
                        let input = input.clone();
                        let on_submit = on_submit.clone();
                        move |_, _, cx| {
                            let text = input.read(cx).value().to_string();
                            on_submit(cx, text);
                            true
                        }
                    });
                if let Some(ok) = &ok_text {
                    dlg = dlg.ok_text(ok.clone());
                }
                dlg
            });
        }
    }
}

/// 把 token 覆盖的字节区间从文本里剥掉，剩下的手输片段拼空格。
/// token spans 已按 range 有序且与文本同步（InputContent 一致性保证）。
fn strip_token_text(text: &str, spans: &[InlineTokenSpan]) -> String {
    if spans.is_empty() {
        return text.trim().to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for span in spans {
        let range = span.range();
        if range.start > last {
            out.push_str(&text[last..range.start]);
            out.push(' ');
        }
        last = last.max(range.end);
    }
    if last < text.len() {
        out.push_str(&text[last..]);
    }
    out.trim().to_string()
}

/// 输入框的「有效查询」：剥掉类型 token 的占位文本，剩用户手输的部分。
/// 粘贴 token 后继续输入文字，过滤就只看这部分。
fn effective_query(input: &InputState) -> String {
    strip_token_text(input.value().as_ref(), input.tokens())
}

/// item 是否命中查询：大小写不敏感的 contains 匹配，空查询匹配一切。
/// 风格对齐 gpui-component 的 `CommandItem::matches`（逐字段 OR），
/// 参与字段：title、id、subtitle、keywords。
fn matches_query(item: &LauncherItem, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let query = query.to_lowercase();

    item.title.to_lowercase().contains(&query)
        || item.id.to_lowercase().contains(&query)
        || item
            .subtitle
            .as_ref()
            .is_some_and(|subtitle| subtitle.to_lowercase().contains(&query))
        || item
            .keywords
            .iter()
            .any(|keyword| keyword.to_lowercase().contains(&query))
}

/// 按「有效查询」（[`matches_query`]）与粘贴类型能力过滤 items，返回可见
/// items 的下标（渲染顺序）。这份列表同时是 `visible_indices`：选中/确认
/// 的 `IndexPath.row` 都是它在列表里的位置。
fn visible_indices(
    items: &[LauncherItem],
    query: &str,
    paste: Option<&PendingPaste>,
) -> Vec<usize> {
    // 图片是惰性识别（还没读字节）：`accepts` 的契约是只按变体声明能力
    // （不看字段值），空字节探针即可判定；确认时读出的才是真内容。
    let image_probe;
    let paste: Option<&PasteContent> = match paste {
        Some(PendingPaste::Ready(content)) => Some(content),
        Some(PendingPaste::Image(_)) => {
            image_probe = PasteContent::Image {
                bytes: Vec::new(),
                format: ImageFormat::Png,
                width: 0,
                height: 0,
            };
            Some(&image_probe)
        }
        None => None,
    };
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            if let Some(content) = paste {
                let Some(handler) = &item.paste else {
                    return false;
                };
                if !(handler.accepts)(content) {
                    return false;
                }
            }
            matches_query(item, query)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Cmd+K 动作面板：自己的 no-search Command + 底部搜索框 + 过滤后的动作列表。
struct ActionsPanel {
    state: Entity<CommandState>,
    input: Entity<InputState>,
    /// 有效查询（剥 token 后的手输文本）。
    query_filter: String,
    /// 过滤条件变更计数（render 依它重置选中；set_selected_index 需要
    /// window，事件回调里没有，统一挪到 render 处理）。
    filter_epoch: u64,
    /// render 已应用过的 filter_epoch。
    applied_filter_epoch: u64,
    /// 上一帧渲染的可见动作下标（row → actions 下标）。
    visible: Vec<usize>,
    actions: Vec<ItemAction>,
}

impl ActionsPanel {
    /// 面板可见动作下标（按 query_filter 的 contains 过滤 title）。
    fn compute_visible(&self) -> Vec<usize> {
        let query = self.query_filter.to_lowercase();
        self.actions
            .iter()
            .enumerate()
            .filter(|(_, action)| query.is_empty() || action.title.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect()
    }
}

pub struct Launcher {
    /// 主搜索框（宿主自有）：Cmd+V 识别、token 渲染、Change 订阅都挂它。
    input: Entity<InputState>,
    /// 有效查询（剥 token 后的手输文本）；随 input 的 Change 同步。
    query_filter: String,
    state: Entity<CommandState>,
    items: Vec<LauncherItem>,
    /// `items` 对应的 `AppShell::items_revision`。
    items_revision: u64,
    /// 过滤条件（query_filter / paste）变更计数；render 依它把选中重置到
    /// 首项。
    filter_epoch: u64,
    /// render 已应用过的 `filter_epoch`。
    applied_filter_epoch: u64,
    /// 上一帧渲染进 Command 的 item 下标（row → items 下标）。确认回调的
    /// IndexPath.row 是这份列表里的位置——粘贴/文本过滤后列表被压缩过，
    /// 不能拿 row 去索引全量 items。
    visible_indices: Vec<usize>,
    /// Cmd+V 识别出的粘贴（图片惰性：只识别类型，字节确认时才读）。Some 时
    /// 列表过滤为声明能处理该类型的 items，回车把内容交给选中 item 的
    /// paste handler。与 on_paste hook 闭包共享（Rc+RefCell）。
    paste: Rc<RefCell<Option<PendingPaste>>>,
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
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search scripts or commands..."));
        // 主输入框变化时重算过滤（用户打字 / 退格删 token）。
        cx.subscribe(&input, |this, _input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.sync_filter(cx)
            }
        })
        .detach();
        // Start with the caret in the search field.
        let focus = input.read(cx).focus_handle(cx);
        focus.focus(window, cx);

        cx.bind_keys([KeyBinding::new("cmd-k", ToggleActions, None)]);

        // Re-collect on every open: scripts and extensions may have changed
        // since the last time the launcher was up. Each module reports its
        // own load errors to the console.
        let CollectedItems {
            items,
            revision: items_revision,
        } = collect_all(cx);
        let item_count = items.len();

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
            input,
            query_filter: String::new(),
            state,
            items,
            items_revision,
            filter_epoch: 0,
            applied_filter_epoch: 0,
            visible_indices: (0..item_count).collect(),
            paste: Rc::new(RefCell::new(None)),
            requests,
            panel: None,
            _blur_task: blur_task,
        }
    }

    fn resolve(&self, section: usize, row: usize) -> Option<&LauncherItem> {
        // 平铺单组：row 直接对应上一帧渲染的可见列表（过滤后已压缩）。
        if section != 0 {
            return None;
        }
        self.visible_indices
            .get(row)
            .and_then(|&index| self.items.get(index))
    }

    /// 主回车确认：读当前选中项走 [`Self::on_confirm`]（空列表无选中则忽略）。
    fn confirm_main(&mut self, window: &mut Window, cx: &mut App) {
        let Some(index) = self.state.read(cx).selected_index() else {
            return;
        };
        self.on_confirm(index, window, cx);
    }

    fn on_confirm(&mut self, index: IndexPath, window: &mut Window, cx: &mut App) {
        let Some(item) = self.resolve(index.section, index.row) else {
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
        // If the receiver is gone the app is shutting down: nothing left to do.
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

    /// ↑/↓：在可见序列里移动选中（`set_selected_index` 自带滚动到可见）。
    fn move_selection(
        &mut self,
        position: usize,
        delta: isize,
        visible_len: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if visible_len == 0 {
            return;
        }
        let next = (position as isize + delta).clamp(0, visible_len as isize - 1) as usize;
        self.state.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(next)), window, cx);
        });
        cx.notify();
    }

    /// Cmd+K：给当前选中项打开/关闭动作面板。
    fn toggle_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.is_some() {
            self.close_actions(window, cx);
            return;
        }
        // 选中项来自主列表的 selection；经 visible_indices 反解回 items。
        let Some(index) = self.state.read(cx).selected_index() else {
            return;
        };
        let Some(item) = self.resolve(index.section, index.row).cloned() else {
            return;
        };
        let state = cx.new(|cx| CommandState::new(window, cx));
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
        self.panel = Some(ActionsPanel {
            state,
            input,
            query_filter: String::new(),
            filter_epoch: 0,
            applied_filter_epoch: 0,
            visible: (0..actions_for(&item).len()).collect(),
            actions: actions_for(&item),
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
            (
                panel.actions.clone(),
                panel.visible.clone(),
                panel.state.read(cx).selected_index(),
            )
        };
        let Some(index) = selected else {
            return;
        };
        let Some(&row) = visible.get(index.row) else {
            return;
        };
        let Some(action) = actions.get(row) else {
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
    fn move_panel_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.as_ref() else {
            return;
        };
        let visible_len = panel.visible.len();
        if visible_len == 0 {
            return;
        }
        let position = panel
            .state
            .read(cx)
            .selected_index()
            .filter(|index| index.section == 0 && index.row < visible_len)
            .map(|index| index.row)
            .unwrap_or(0);
        let next = (position as isize + delta).clamp(0, visible_len as isize - 1) as usize;
        panel.state.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(next)), window, cx);
        });
        cx.notify();
    }
}

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        // A weak handle: the closure is stored in CommandState (a child of
        // this view), so a strong one would keep both entities alive forever.
        let this = cx.entity().downgrade();

        // 过滤条件变更后把选中重置到首项（set_selected_index 需要 window，
        // 事件回调里没有，统一挪到 render 处理）。
        if self.applied_filter_epoch != self.filter_epoch {
            self.applied_filter_epoch = self.filter_epoch;
            let empty = {
                let paste = self.paste.borrow();
                visible_indices(&self.items, &self.query_filter, paste.as_ref()).is_empty()
            };
            self.state.update(cx, |state, cx| {
                state.set_selected_index(
                    if empty { None } else { Some(IndexPath::new(0)) },
                    window,
                    cx,
                );
            });
        }

        let visible = {
            // 只在借用窗口内过滤（PendingPaste::Ready 里带字节，不能 clone）。
            let paste = self.paste.borrow();
            visible_indices(&self.items, &self.query_filter, paste.as_ref())
        };
        let visible_count = visible.len();
        self.visible_indices = visible;

        let mut command = Command::new(&self.state)
            // 宿主自定义搜索框：关掉内置搜索框与内置文本过滤——no-search
            // 模式下 Command 对喂进去的 items 不做任何过滤，所以下面只把
            // 过滤后的可见项放进 model（过滤在上方 visible_indices 做完）。
            .searchable(false)
            // Fill the window so the list (flex_1) stretches instead of
            // leaving a blank strip below the footer; raise the list's cap
            // accordingly (default 18.75rem is shorter than this window).
            .h_full()
            .max_h(rems(40.))
            .on_confirm(move |index, window, app| {
                let _ = this.update(app, |this, cx| this.on_confirm(index, window, cx));
            })
            .footer(move |_, _window, cx| {
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{visible_count} 个命令"))
                    .into_any_element()
            });

        if !self.visible_indices.is_empty() {
            // 平铺：单组、无组头（label 是 Option，不设就不渲染分组标题）；
            // 只放可见项，model 里的 row == visible_indices 里的位置。
            let mut group = CommandGroup::new();
            for &index in &self.visible_indices {
                let item = &self.items[index];
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
                    });
                group = group.item(entry);
            }
            command = command.group(group);
        }

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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(key, "up" | "down") || event.keystroke.modifiers.modified() {
                    return;
                }
                cx.stop_propagation();
                let delta = if key == "up" { -1 } else { 1 };
                if this.panel.is_some() {
                    this.move_panel_selection(delta, window, cx);
                } else {
                    let position = this
                        .state
                        .read(cx)
                        .selected_index()
                        .filter(|index| index.section == 0)
                        .map(|index| index.row)
                        .unwrap_or(0);
                    let visible_len = this.visible_indices.len();
                    this.move_selection(position, delta, visible_len, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleActions, window, cx| {
                this.toggle_actions(window, cx);
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(div().px_3().pt_3().child(search_input))
                    .child(command),
            );

        // Cmd+K 动作面板：右下角浮层，列表在上、搜索框在底部（对齐 Raycast）。
        if let Some(panel) = &mut self.panel {
            // 过滤条件变更后把面板选中重置到首项。
            if panel.applied_filter_epoch != panel.filter_epoch {
                panel.applied_filter_epoch = panel.filter_epoch;
                let empty = panel.compute_visible().is_empty();
                panel.state.update(cx, |state, cx| {
                    state.set_selected_index(
                        if empty { None } else { Some(IndexPath::new(0)) },
                        window,
                        cx,
                    );
                });
            }
            panel.visible = panel.compute_visible();
            let visible_rows = panel.visible.clone();
            let actions = panel.actions.clone();
            let requests = self.requests.clone();
            let mut panel_command = Command::new(&panel.state)
                .searchable(false)
                .max_h(rems(22.))
                .on_confirm(move |index, window, _app| {
                    let Some(&row) = visible_rows.get(index.row) else {
                        return;
                    };
                    let Some(action) = actions.get(row) else {
                        return;
                    };
                    // 展示/编辑型动作：直接弹 AlertDialog，不关窗口（面板保留）。
                    // 执行型动作：经 channel 发 LauncherAction，然后关窗口。
                    if let Some(spec) = &action.dialog {
                        open_action_dialog(window, _app, spec);
                    } else {
                        let _ = requests.send(action.run.clone());
                        window.remove_window();
                    }
                });

            let mut group = CommandGroup::new();
            for &row in &panel.visible {
                let action = &panel.actions[row];
                let title = action.title.clone();
                let shortcut = action.shortcut;
                group = group.item(CommandItem::new().label(action.title.clone()).child(
                    move |_, cx| {
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
                    },
                ));
            }
            panel_command = panel_command.group(group);

            let panel_input = Input::new(&panel.input)
                .appearance(false)
                .prefix(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground));

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
                    .overflow_hidden()
                    .child(
                        v_flex().child(panel_command).child(
                            div()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .p_2()
                                .child(panel_input),
                        ),
                    ),
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
    use gpui_kit::base::input::{InlineToken, InputContent};
    use std::sync::Arc;

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
    fn actions_fallback_has_open_plus_keyword_actions() {
        let mut it = item("a", "内置");
        let actions = super::actions_for(&it);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].title, "打开");
        assert_eq!(actions[0].shortcut, Some("↵"));
        // 两条关键词动作：展示（dialog 只读）与编辑（dialog 带输入框）。
        assert_eq!(actions[1].title, "显示搜索关键词");
        assert!(matches!(
            actions[1].dialog,
            Some(super::DialogSpec::Info { .. })
        ));
        assert_eq!(actions[2].title, "编辑搜索关键词");
        assert!(matches!(
            actions[2].dialog,
            Some(super::DialogSpec::Edit { .. })
        ));
        // 编辑框初始值是 item 当前的 keywords。
        it.keywords = vec!["foo".into(), "bar".into()];
        let actions = super::actions_for(&it);
        let Some(super::DialogSpec::Edit { initial, .. }) = actions[2].dialog.clone() else {
            panic!("编辑动作应携带 Edit dialog");
        };
        assert_eq!(initial, "foo, bar");
    }

    #[test]
    fn declared_actions_are_preserved_then_keyword_actions_appended() {
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
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0].title, "第一个");
        assert_eq!(actions[1].title, "第二个");
        assert_eq!(actions[2].title, "显示搜索关键词");
        assert_eq!(actions[3].title, "编辑搜索关键词");
    }

    #[test]
    fn parse_keywords_splits_and_trims() {
        use super::usage;
        assert_eq!(
            usage::parse_keywords("微信, wechat，pin"),
            vec!["微信".to_string(), "wechat".to_string(), "pin".to_string()]
        );
        assert!(usage::parse_keywords("  , ， ").is_empty());
    }

    #[test]
    fn visible_indices_filters_by_query_and_paste_capability() {
        use crate::ui::clipboard::{PasteContent, PendingPaste};
        let mut pinnable = item("pin", "内置");
        pinnable.paste = Some(super::item::PasteHandler {
            accepts: |content: &PasteContent| matches!(content, PasteContent::Image { .. }),
            run: Arc::new(|_, _| {}),
        });
        let plain = item("app", "应用");
        let items = vec![pinnable, plain];

        // 无过滤：全部可见。
        assert_eq!(super::visible_indices(&items, "", None), vec![0, 1]);
        // 文本过滤：title/id/keywords 任一包含（大小写不敏感）。
        assert_eq!(super::visible_indices(&items, "APP", None), vec![1]);
        // 粘贴图片（惰性：只有类型、空字节探针）：只有声明能处理图片的 pin
        // 可见——顺带验证 accepts 按变体匹配的契约。
        assert_eq!(
            super::visible_indices(&items, "", Some(&PendingPaste::Image(None))),
            vec![0]
        );
        // 粘贴 + 文本叠加。
        assert_eq!(
            super::visible_indices(&items, "pin", Some(&PendingPaste::Image(None))),
            vec![0]
        );
        // 已读出的非图片内容（Ready）：pin 不接受。
        assert!(
            super::visible_indices(
                &items,
                "",
                Some(&PendingPaste::Ready(PasteContent::Text("x".into())))
            )
            .is_empty()
        );
    }

    #[test]
    fn strip_token_text_keeps_hand_typed_parts() {
        // 无 token：trim 后的原文。
        assert_eq!(super::strip_token_text(" pin ", &[]), "pin");
        // token 覆盖「图片」（6 字节）+ 手输 "pin"：剥掉 token 段。
        let content = InputContent::new("图片")
            .with_token(0..6, InlineToken::new("json", "图片").with_label("JSON"));
        let input = content.expect("token covers its own text");
        assert_eq!(super::strip_token_text("图片pin", input.tokens()), "pin");
        // 只有 token：空查询。
        let only = InputContent::new("图片")
            .with_token(0..6, InlineToken::new("json", "图片").with_label("JSON"));
        assert_eq!(
            super::strip_token_text("图片", only.expect("valid").tokens()),
            ""
        );
    }
}
