//! 标题栏按钮的 tooltip。
//!
//! gpui-component 的 `Button::tooltip` 只认**注册在 `gpui_base::Root` 上的
//! `WindowState` 插件**（`WindowState::entity` → `window.root::<base::Root>()`
//! 再取插件）。普通窗口由 `gpui_kit::open_window` 包了 `base::Root`，插件装得上，
//! tooltip 正常；插件 UI 窗口的根视图是 `gpui_shell::ShellRoot`，那层插件挂不上，
//! hover 时 `WindowState::tooltip_overlay` 返回 `None`，tooltip 就**静默不显示**。
//!
//! gpui-shell 自己也有一个 tooltip 层（`ShellRoot::tooltip_overlay`，用的是同一个
//! `render_tooltip` 渲染器，视觉与普通窗口一致）。[`button`] 在渲染期看一眼根视图
//! 类型：普通窗口照旧交给 `Button::tooltip`，ShellRoot 窗口改向 shell 的 tooltip 层
//! 发请求。调用方不用关心自己身处在哪种窗口里。

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::{ElementExt as _, TooltipRequest};
use gpui_kit::component::button::Button;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, Bounds, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, SharedString, StatefulInteractiveElement as _, Window, div,
};

/// 给标题栏按钮挂 tooltip；`window` 只用来判断根视图类型（渲染期只读）。
///
/// `id` 只用于包装层的 hover 追踪，同窗口内必须唯一（按钮自己的 id 即可，
/// 前面加个前缀避免与按钮撞车）。
pub(crate) fn button(
    id: impl Into<ElementId>,
    button: Button,
    tooltip: impl Into<SharedString>,
    window: &Window,
) -> AnyElement {
    let tooltip = tooltip.into();
    if window.root::<gpui_shell::ShellRoot>().flatten().is_none() {
        return button.tooltip(tooltip).into_any_element();
    }

    // ShellRoot 窗口：按钮自己不带 tooltip，外面包一层接管 hover。
    // 锚点用 prepaint 拿到的真实布局（和 gpui-component 内部做法一致）。
    let bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let measure = bounds.clone();
    let anchor = bounds.clone();
    div()
        .id(id)
        .on_prepaint(move |resolved, _, _| measure.set(resolved))
        .on_hover(move |hovered, window, cx| {
            let Some(overlay) = gpui_shell::ShellRoot::tooltip_overlay(window, cx) else {
                eprintln!("[TEMP-VERIFY] hover={hovered} 无 shell tooltip 层");
                return;
            };
            eprintln!("[TEMP-VERIFY] hover={hovered} 有 shell tooltip 层");
            overlay.update(cx, |overlay, cx| {
                if *hovered {
                    let tooltip = tooltip.clone();
                    overlay.request_show(
                        TooltipRequest::new(anchor.get(), move |window, cx| {
                            Tooltip::new(tooltip.clone()).build(window, cx)
                        }),
                        window,
                        cx,
                    );
                } else {
                    overlay.request_hide(window, cx);
                }
            });
        })
        .child(button)
        .into_any_element()
}
