//! 宿主统一自绘标题栏：13px semibold 主标题 +（可选）11px muted 副标题竖排，
//! 右侧 trailing 控件（动作按钮、置顶图钉）。内部包 `gpui_kit::component::TitleBar`
//! ——拖动、双击跟随系统设置、`app_owns_titlebar_drag` 等都由它负责，本层只
//! 统一「标题块长什么样、右侧控件怎么排」。
//!
//! 约束：**每个窗口只渲染一条**（`TitleBar` 内部经 `window.use_state` 按调用
//! 点共享拖动状态，同一窗口出现两条会互相干扰）。
//!
//! 布局机制：`TitleBar` 内部的 children 容器是 `justify_between`，所以本层
//! 最多放两个 child——左侧标题块（`flex_shrink_0` 防挤压）、右侧控件行。调用方
//! **不需要** `flex_1` spacer；多于两个 trailing 控件会被包进同一个行容器。

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, TitleBar};
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div, px,
};

use super::pin::WindowPin;

#[derive(IntoElement)]
pub(crate) struct WindowTitleBar {
    title: SharedString,
    subtitle: Option<SharedString>,
    /// 右侧控件（动作按钮、图钉），按 push 顺序从左到右排；约定图钉最后。
    trailing: Vec<AnyElement>,
    style: StyleRefinement,
}

impl WindowTitleBar {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            trailing: Vec::new(),
            style: StyleRefinement::default(),
        }
    }

    /// 标题下方第二行（11px muted）。
    pub(crate) fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// 追加一个右侧控件。多个控件按调用顺序水平排列（图钉约定最后）。
    pub(crate) fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing.push(element.into_any_element());
        self
    }

    /// 便捷方法：挂公共置顶图钉。
    pub(crate) fn pin(self, pin: gpui_kit::Entity<WindowPin>) -> Self {
        self.trailing(pin)
    }
}

impl Styled for WindowTitleBar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for WindowTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;

        let mut title = div()
            .flex()
            // .flex_col()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(colors.foreground)
                    .child(self.title),
            );
        if let Some(subtitle) = self.subtitle {
            title = title.child(
                div()
                    .ml_2()
                    .text_size(px(11.))
                    .text_color(colors.muted_foreground)
                    .child(subtitle),
            );
        }

        let mut bar = TitleBar::new().child(title);
        // trailing 控件必须包进一个行容器：#bar 是 justify_between，超过两个
        // child 会被摊开而不是靠右。
        if !self.trailing.is_empty() {
            let mut row = div()
                .flex()
                .items_center()
                .gap(px(4.))
                .px_2()
                .flex_shrink_0();
            for element in self.trailing {
                row = row.child(element);
            }
            bar = bar.child(row);
        }
        // 调用方的 .h()/.bg()/.pl() 经 Styled 透传，refine 在默认样式之后，
        // 一定能覆盖 `#title-bar` 的默认高度/背景/左内边距。
        bar.refine_style(&self.style)
    }
}
