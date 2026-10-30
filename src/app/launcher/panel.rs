//! Cmd+K 动作面板：自己的 no-search Command + 底部搜索框 + 过滤后的动作列表。
//!
//! 面板是 Launcher 主视图上的一个浮层（右下角，Raycast 风格）：列表在上、
//! 搜索框在底部。查询过滤只看动作 title，语义与主列表一致（空格分词、逐词 AND）。

use crossbeam_channel::Sender;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::IndexPath;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Div, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _,
    Window, div, px, rems,
};

use gpui_kit::assets::IconName;

use super::actions::open_action_dialog;
use super::filter::matches_query_terms;
use super::item::{ItemAction, LauncherAction};

/// Cmd+K 动作面板的状态。
pub(crate) struct ActionsPanel {
    pub state: Entity<CommandState>,
    pub input: Entity<InputState>,
    /// 有效查询（剥 token 后的手输文本）。
    pub query_filter: String,
    /// 过滤条件变更计数（render 依它重置选中；set_selected_index 需要
    /// window，事件回调里没有，统一挪到 render 处理）。
    pub filter_epoch: u64,
    /// render 已应用过的 filter_epoch。
    pub applied_filter_epoch: u64,
    /// 上一帧渲染的可见动作下标（row → actions 下标）。
    pub visible: Vec<usize>,
    pub actions: Vec<ItemAction>,
}

impl ActionsPanel {
    /// 面板可见动作下标：与主列表同一套语义（空格分词、逐词 AND），
    /// 只是参与匹配的字段只有 title。
    pub fn compute_visible(&self) -> Vec<usize> {
        self.actions
            .iter()
            .enumerate()
            .filter(|(_, action)| matches_query_terms(&self.query_filter, &[action.title.as_str()]))
            .map(|(index, _)| index)
            .collect()
    }
}

/// 渲染动作面板浮层：先处理过滤 epoch 的选中重置，再构建 Command 与底部
/// 搜索框。返回的 div 由调用方挂到 launcher root 上。
pub(crate) fn render_overlay(
    panel: &mut ActionsPanel,
    requests: &Sender<LauncherAction>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
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
    let requests = requests.clone();
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
        group = group.item(
            CommandItem::new()
                .label(action.title.clone())
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

    let panel_input = Input::new(&panel.input)
        .appearance(false)
        .prefix(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground));

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
        )
}
