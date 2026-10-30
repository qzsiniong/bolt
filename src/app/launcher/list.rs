//! 命令列表的行与容器：Launcher 主列表与 Cmd+K 动作面板共用。
//!
//! 交互语义按 Raycast：**hover 只做淡高亮，不动选中**（鼠标掠过不会把键盘
//! 选中的那一行抢走）、**单击选中**、**双击执行**（等同回车）。
//!
//! 没有直接用 gpui-component 的 `Command`：它的行走固定逻辑
//! `on_hover → select` + `on_click → confirm`（`command/state.rs`），悬停即
//! 选中、单击就执行，而且没有开关。行内容本来就是自绘的，容器也就自己来。

use gpui_kit::base::v_flex;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, DefiniteLength, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};

/// 这次点击是**执行**（双击）还是**只选中**（单击）。
pub(crate) fn is_activation(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => click.down.click_count >= 2,
        ClickEvent::Touch(click) => click.tap_count >= 2,
        // 键盘/辅助功能触发的 click 没有连击概念，一次就算执行。
        ClickEvent::Keyboard(_) => true,
    }
}

/// 一行：整行可点；选中态用 accent 底色，未选中时 hover 只给一层淡底。
///
/// `index` 既是行下标也用来生成本行 id（同一容器内唯一）。
pub(crate) fn row(
    index: usize,
    selected: bool,
    content: AnyElement,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let colors = cx.theme().colors;
    div()
        .id(("command-row", index))
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap_2()
        .px_2()
        .py_1p5()
        .text_sm()
        .rounded(px(6.))
        .when(selected, |this| {
            this.bg(colors.accent).text_color(colors.accent_foreground)
        })
        // hover 纯粹是视觉反馈：选中仍然只由单击和 ↑/↓ 决定。
        .when(!selected, |this| {
            this.hover(move |style| style.bg(colors.list_hover))
        })
        .on_click(on_click)
        .child(content)
        .into_any_element()
}

/// 可滚动列表容器。
///
/// - `max_h` 为 None：主列表，`flex_1 + min_h_0` 吃掉窗口剩余高度；
/// - `max_h` 为 Some：浮层里的列表，按内容撑开但不超过上限，超出就滚。
///
/// `track_scroll` 记下每行的位置，调用方用 [`ScrollHandle::scroll_to_item`]
/// 就能把键盘选中的行带进视野。
pub(crate) fn container(
    id: &'static str,
    scroll: &ScrollHandle,
    max_h: Option<DefiniteLength>,
    rows: Vec<AnyElement>,
) -> AnyElement {
    v_flex()
        .id(id)
        .p_1()
        .when(max_h.is_some(), |this| this.flex_none())
        .when(max_h.is_none(), |this| this.flex_1().min_h_0())
        .when_some(max_h, |this, max_h| this.max_h(max_h))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .children(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{MouseClickEvent, MouseDownEvent, TouchClickEvent};

    fn mouse(click_count: usize) -> ClickEvent {
        ClickEvent::Mouse(MouseClickEvent {
            down: MouseDownEvent {
                click_count,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn single_click_only_selects_and_double_click_activates() {
        assert!(!is_activation(&mouse(1)));
        assert!(is_activation(&mouse(2)));
        // 三连击也得算执行，别掉回"只选中"。
        assert!(is_activation(&mouse(3)));
        // 触屏双击同理。
        assert!(is_activation(&ClickEvent::Touch(TouchClickEvent {
            tap_count: 2,
            ..Default::default()
        })));
    }
}
