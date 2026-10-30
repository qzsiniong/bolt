//! Cmd+K 动作面板：底部搜索框 + 过滤后的动作列表（右下角浮层，Raycast 风格）。
//!
//! 列表与主列表共用 [`list`](super::list)：hover 只淡高亮、单击选中、双击执行。
//! 查询过滤只看动作 title，语义与主列表一致（空格分词、逐词 AND）。

use crossbeam_channel::Sender;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Div, Entity, Focusable as _, InteractiveElement as _, IntoElement as _,
    ParentElement as _, ScrollHandle, Styled as _, WeakEntity, div, px, rems,
};

use gpui_kit::assets::IconName;

use super::Launcher;
use super::actions::open_action_dialog;
use super::filter::matches_query_terms;
use super::item::{ItemAction, LauncherAction};
use super::list;

/// Cmd+K 动作面板的状态。
pub(crate) struct ActionsPanel {
    pub input: Entity<InputState>,
    /// 有效查询（剥 token 后的手输文本）。
    pub query_filter: String,
    /// 过滤条件变更计数（render 依它重置选中）。
    pub filter_epoch: u64,
    /// render 已应用过的 filter_epoch。
    pub applied_filter_epoch: u64,
    /// 上一帧渲染的可见动作下标（row → actions 下标）。
    pub visible: Vec<usize>,
    /// 选中行（`visible` 里的下标）；None = 没有可执行的动作。
    pub selected: Option<usize>,
    /// 面板列表的滚动句柄：↑/↓ 之后把选中行带进视野。
    pub scroll: ScrollHandle,
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

/// 渲染动作面板浮层：先处理过滤 epoch 的选中重置，再构建列表与底部搜索框。
/// 返回的 div 由调用方挂到 launcher root 上。
pub(crate) fn render_overlay(
    panel: &mut ActionsPanel,
    requests: &Sender<LauncherAction>,
    launcher: WeakEntity<Launcher>,
    cx: &mut App,
) -> Div {
    // 过滤条件变更后把面板选中重置到首项。
    if panel.applied_filter_epoch != panel.filter_epoch {
        panel.applied_filter_epoch = panel.filter_epoch;
        let empty = panel.compute_visible().is_empty();
        panel.selected = (!empty).then_some(0);
        panel.scroll.scroll_to_item(0);
    }
    panel.visible = panel.compute_visible();

    let mut rows = Vec::with_capacity(panel.visible.len());
    for (row, &index) in panel.visible.iter().enumerate() {
        let action = &panel.actions[index];
        let title = action.title.clone();
        let shortcut = action.shortcut;
        let muted = cx.theme().muted_foreground;
        let content = h_flex()
            .flex_1()
            .items_center()
            .justify_between()
            .child(div().child(title))
            .when_some(shortcut, |this, s| {
                this.child(div().text_sm().text_color(muted).child(s))
            })
            .into_any_element();

        // 点击回调要在 click 时脱离这次 render 的借用：把需要的东西先克隆出来。
        let spec = action.dialog.clone();
        let run = action.run.clone();
        let requests = requests.clone();
        let launcher = launcher.clone();
        rows.push(list::row(
            row,
            panel.selected == Some(row),
            content,
            cx,
            move |event, window, cx| {
                if list::is_activation(event) {
                    // 双击执行：与主列表一致（Enter 也走同一套 action）。
                    if let Some(spec) = &spec {
                        // 展示/编辑型动作：弹 AlertDialog，面板保留。
                        open_action_dialog(window, cx, spec);
                    } else {
                        let _ = requests.send(run.clone());
                        // 关窗口延到本轮事件派发之后（旧 Command 的 confirm 同样 defer）。
                        window.defer(cx, |window, _| window.remove_window());
                    }
                } else {
                    // 单击只选中，焦点留在面板搜索框。
                    launcher
                        .update(cx, |this, cx| {
                            let Some(panel) = this.panel.as_mut() else {
                                return;
                            };
                            panel.selected = Some(row);
                            panel.scroll.scroll_to_item(row);
                            let focus = panel.input.read(cx).focus_handle(cx);
                            focus.focus(window, cx);
                            cx.notify();
                        })
                        .ok();
                }
            },
        ));
    }
    let list = list::container("actions-list", &panel.scroll, Some(rems(22.).into()), rows);

    let panel_input = Input::new(&panel.input)
        .appearance(false)
        .prefix(Icon::new(IconName::Search).text_color(cx.theme().muted_foreground));

    div()
        .absolute()
        .bottom_4()
        .right_4()
        // 注册遮挡 hitbox，阻止 hover/滚动穿透到底层的主列表。
        .occlude()
        .w(px(420.))
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().popover)
        .shadow_lg()
        .overflow_hidden()
        .child(
            v_flex().max_h(rems(22.)).child(list).child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_2()
                    .child(panel_input),
            ),
        )
}
