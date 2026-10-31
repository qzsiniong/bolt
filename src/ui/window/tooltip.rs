//! 标题栏按钮的 tooltip。
//!
//! 不能直接用 gpui-component 的 `Button::tooltip`：它只认**注册在 `gpui_base::Root`
//! 上的 `WindowState` 扩展**（`WindowState::entity` → `window.root::<base::Root>()`
//! 再取扩展）。普通窗口由 `gpui_kit::open_window` 包了 `base::Root`，扩展装得上；
//! 扩展 UI 窗口的根视图是 `gpui_shell::ShellRoot`，那层扩展挂不上，hover 时取不到
//! tooltip 层，于是**静默不显示**。
//!
//! 这里改用 gpui 自带的 tooltip（`Window` 自己绘制，见 `Window::prepaint_tooltip`）：
//! 它与根视图类型无关，所有窗口一套行为；内容仍用 gpui-component 的 `Tooltip`
//! 视图，视觉和其他窗口一致。

use gpui_kit::component::button::Button;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, div,
};

/// 给标题栏按钮挂 tooltip 并返回元素。
///
/// `id` 只用于包装层的 hover 追踪，同一窗口内需唯一（按钮自己的 id 加个前缀）。
pub(crate) fn button(
    id: impl Into<ElementId>,
    button: Button,
    tooltip: impl Into<SharedString>,
) -> AnyElement {
    let tooltip = tooltip.into();
    div()
        .id(id)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(button)
        .into_any_element()
}
