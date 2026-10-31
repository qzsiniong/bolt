//! 标题栏右上角的置顶图钉。
//!
//! **无自有状态**：渲染时读窗口真实层级（[`is_pinned`](super::level)，见
//! `level.rs` 的模块文档），点击翻转层级后 `notify` 重绘。默认置顶的窗口由
//! 开窗侧先 `set_pinned(true)`，图钉读到的自然就是 true——「层级」与「图标
//! 显示的状态」是同一个东西，不存在人工同步，也就不可能漂移。
//!
//! 它是一个实体（而不是纯函数返回元素）的唯一原因：点击后需要 `cx.notify()`
//! 触发自身重绘。

use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::{
    Context, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    Styled, Window, div,
};

use super::level::{is_pinned, set_pinned};

pub(crate) struct WindowPin;

impl WindowPin {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Render for WindowPin {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pinned = is_pinned(window);
        let (icon, tooltip) = if pinned {
            (IconName::PinOff, "取消置顶")
        } else {
            (IconName::Pin, "置顶")
        };
        // 元素 id 由「固定前缀 + 本实体 id」组成：同一窗口里放多个图钉也不会撞 id。
        let id = ElementId::from(("window-pin", cx.entity_id()));
        div()
            .id(id.clone())
            .flex()
            .items_center()
            .justify_end()
            // .px_2()
            // 自绘标题栏整条都是拖动区，按下事件拦下来，避免拖动时按钮进入
            // 按压态。代价：图钉这一小块不能用来拖窗——可接受。
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(super::button_with_tooltip(
                ("window-pin-tooltip", cx.entity_id()),
                Button::new(id)
                    .icon(icon)
                    .small()
                    .ghost()
                    .on_click(cx.listener(|_, _, window, cx| {
                        set_pinned(window, !is_pinned(window));
                        cx.notify();
                    })),
                tooltip,
            ))
    }
}
