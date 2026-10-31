//! 开窗样板收敛：`WindowSpec` → `WindowOptions`（以 `TitleBar::window_options()`
//! 为基底），以及「激活已有 / 失效重建」的单例语义。
//!
//! **两条开窗通道，别混**：
//! - [`open_spec`] / [`open_singleton`] 走 `gpui_kit::open_window`，根视图外面会包一层
//!   `base::Root`（主题注入等），普通内容窗口都用它。
//! - 根视图必须是 `gpui_shell::ShellRoot` 的扩展 UI 窗口**只能**裸 `cx.open_window`
//!   （包了 Root，脚本侧的 tooltip / toast / dialog 会静默失效），它只复用
//!   [`WindowSpec::to_options`] 与 `set_pinned`，不用本文件的 open 函数。
//!
//! 与窗口标题相关的两层职责：`WindowSpec.title` 进 `TitlebarOptions.title`
//! （Mission Control / Cmd+Tab 里的系统级标题，开窗时生效）；自绘标题栏里
//! 的标题/副标题是展示层，运行时可变（如脚本管理器逐帧 `set_window_title`）。

use gpui_kit::component::TitleBar;
use gpui_kit::{
    AnyWindowHandle, App, Bounds, Entity, Render, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowKind, WindowOptions, px, size,
};

use super::level::set_pinned;

/// 自绘标题栏窗口的开窗参数（构建器）。
///
/// `pinned` 是「默认置顶」的唯一声明处：[`open_spec`] 会在创建视图**之前**
/// 把层级抬到 floating，视图里的 [`super::WindowPin`] 随后读到的就是真值。
pub(crate) struct WindowSpec {
    /// 系统级窗口标题（`TitlebarOptions.title`）。
    title: SharedString,
    /// 初始尺寸；`None` = 不指定 `window_bounds`（系统默认位置与尺寸）。
    size: Option<(f32, f32)>,
    min_size: Option<(f32, f32)>,
    pinned: bool,
    focus: bool,
}

impl WindowSpec {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            size: None,
            min_size: None,
            pinned: false,
            focus: true,
        }
    }

    /// 初始尺寸（窗口居中弹出）。
    pub(crate) fn size(mut self, width: f32, height: f32) -> Self {
        self.size = Some((width, height));
        self
    }

    /// 最小尺寸（`window_min_size`）。
    pub(crate) fn min_size(mut self, width: f32, height: f32) -> Self {
        self.min_size = Some((width, height));
        self
    }

    /// 默认置顶：开窗即 floating 层级（用户仍可点图钉切换）。
    pub(crate) fn pinned(mut self, pinned: bool) -> Self {
        self.pinned = pinned;
        self
    }

    /// 纯映射（可单测）。置顶**不进 options**——层级是运行时消息，由
    /// [`open_spec`] 在 build 前设置，避免「开窗设一次、图钉再设一次」两处真相。
    pub(crate) fn to_options(&self, cx: &App) -> WindowOptions {
        let mut options = WindowOptions {
            kind: WindowKind::Normal,
            window_min_size: self.min_size.map(|(w, h)| size(px(w), px(h))),
            titlebar: Some(TitlebarOptions {
                title: Some(self.title.clone()),
                ..TitleBar::title_bar_options()
            }),
            is_resizable: true,
            focus: self.focus,
            // `TitleBar::window_options()` 给出「自绘标题栏自己处理拖动与双击、
            // AppKit 不再把它当系统拖动区」的开窗参数（`app_owns_titlebar_drag`）。
            ..TitleBar::window_options()
        };
        if let Some((w, h)) = self.size {
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(w), px(h)),
                cx,
            )));
        }
        options
    }
}

/// 按 spec 开窗。置顶在 build 闭包内、视图创建之前设置——视图里的
/// `WindowPin` 首帧读到的层级即真值。
pub(crate) fn open_spec<V: Render>(
    spec: &WindowSpec,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> gpui_kit::Result<(AnyWindowHandle, Entity<V>)> {
    // 从 launcher 动作里开窗时 launcher 正关着，不抢一次前台会被压到别的
    // App 后面（gpui 只在开窗时 makeKeyAndOrderFront，从不 NSApp.activate）。
    cx.activate(true);
    let pinned = spec.pinned;
    gpui_kit::open_window(spec.to_options(cx), cx, move |window, cx| {
        if pinned {
            set_pinned(window, true);
        }
        build(window, cx)
    })
}

/// 单例开窗：`existing` 里的句柄还活着就激活它并返回，失效则按 spec 重建。
/// 句柄的存取（各模块 Global 里的 `Option<AnyWindowHandle>`）仍由调用方持有，
/// 这里只封装「激活 or 新建」的判定。
///
/// 激活已有窗口时**带副作用**的入口（如脚本管理器激活时触发新建脚本流程）
/// 请自管既有句柄逻辑，直接用 [`open_spec`]。
pub(crate) fn open_singleton<V: Render>(
    spec: &WindowSpec,
    cx: &mut App,
    existing: Option<AnyWindowHandle>,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> gpui_kit::Result<AnyWindowHandle> {
    if let Some(handle) = existing
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return Ok(handle);
    }
    open_spec(spec, cx, build).map(|(handle, _view)| handle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui_kit::test]
    fn options_follow_the_title_bar_conventions(cx: &mut gpui_kit::App) {
        let spec = WindowSpec::new("测试窗口")
            .size(900., 560.)
            .min_size(560., 360.)
            .pinned(true);
        let options = spec.to_options(cx);
        // 自绘标题栏的开窗基底：透明系统标题栏 + 红绿灯 (9,9)，拖动归应用管。
        let titlebar = options.titlebar.expect("titlebar must be set");
        assert!(titlebar.appears_transparent);
        assert_eq!(
            titlebar.traffic_light_position,
            Some(gpui_kit::point(gpui_kit::px(9.), gpui_kit::px(9.)))
        );
        assert!(options.app_owns_titlebar_drag);
        assert_eq!(options.kind, WindowKind::Normal);
        assert!(options.is_resizable);
        assert!(options.focus);
        let Some(gpui_kit::WindowBounds::Windowed(bounds)) = options.window_bounds else {
            panic!("window_bounds must be Windowed");
        };
        assert_eq!(
            bounds.size,
            gpui_kit::size(gpui_kit::px(900.), gpui_kit::px(560.))
        );
        assert_eq!(
            options.window_min_size,
            Some(gpui_kit::size(gpui_kit::px(560.), gpui_kit::px(360.)))
        );
    }

    #[gpui_kit::test]
    fn unsized_spec_leaves_the_bounds_to_the_system(cx: &mut gpui_kit::App) {
        let spec = WindowSpec::new("无尺寸窗口");
        let options = spec.to_options(cx);
        assert!(options.window_bounds.is_none());
        assert!(options.window_min_size.is_none());
        assert!(options.is_resizable);
    }
}
