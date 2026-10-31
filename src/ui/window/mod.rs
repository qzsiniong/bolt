//! 窗口公共设施：自绘标题栏（[`WindowTitleBar`]）、置顶图钉（[`WindowPin`]）、
//! 窗口层级 FFI（`set_pinned` / `is_pinned` / `move_origin`）与开窗样板
//! （[`WindowSpec`] / [`open_spec`] / [`open_singleton`]）。
//!
//! 三步用法（以「默认置顶的独立窗口」为例）：
//! 1. 开窗：`WindowSpec::new("Console").size(900., 560.).pinned(true)` →
//!    [`open_singleton`]（单例）或 [`open_spec`]（每次新建）。
//! 2. 视图持有图钉：`cx.new(|_| WindowPin::new())`——它无状态，首帧读到的
//!    就是开窗时设好的层级。
//! 3. 渲染标题栏：`WindowTitleBar::new("Console").subtitle("..").pin(pin.clone())`。
//!
//! ## 哪些窗口不参与（以及为什么）
//!
//! | 窗口 | 原因 |
//! | --- | --- |
//! | Launcher（`app/windows.rs`） | `WindowKind::Floating` 无边框面板、toggle 语义（存在即关而非激活），没有标题栏概念 |
//! | 通知（`ui/notify_window.rs`）、取色（`color_picker/window.rs`） | `PopUp` + FFI 改 styleMask 的特殊窗口（透明/覆盖屏），无标题栏 |
//! | 交互浮窗（`runtime/interaction_window_manager.rs`） | `PopUp` 非激活面板，保持系统标题栏；本身就是临时浮层，置顶无意义 |
//! | 扩展 UI 窗口的根视图 | 根视图必须是 `gpui_shell::ShellRoot`（tooltip/toast/dialog 依赖它），只能裸 `cx.open_window`；标题栏作为它的 content |
//! | 便签（`modules/pin/`） | 无标题栏的 borderless 窗口，保留黑底圆形按钮的自有视觉；但置顶状态同样走本模块的 `level`（同一真相） |
//!
//! ## 默认置顶策略
//!
//! Console 独立窗口默认置顶（调试时的常驻仪表盘）；脚本管理器、JSON 编辑器、
//! 扩展管理、新建扩展默认不置顶；扩展 UI 窗口由清单 `ui.always_on_top` 决定。
//! 所有默认值都写在各模块的 `window.rs`（`WindowSpec.pinned(...)`），运行时
//! 用户点图钉随时切换。
//!
//! ## 标题栏按钮的 tooltip
//!
//! 一律用 [`button_with_tooltip`]：它包一层 gpui 原生 tooltip，与窗口根视图类型
//! 无关。**不要**用 gpui-component 的 `Button::tooltip`——它依赖注册在
//! `gpui_base::Root` 上的组件插件，根视图是 `gpui_shell::ShellRoot` 的插件 UI
//! 窗口里会静默不显示（细节见 `tooltip.rs` 的模块文档）。

mod level;
pub(crate) mod open;
pub(crate) mod pin;
pub(crate) mod title_bar;
mod tooltip;

pub(crate) use open::{WindowSpec, open_singleton, open_spec};
pub(crate) use pin::WindowPin;
pub(crate) use title_bar::WindowTitleBar;
pub(crate) use tooltip::button as button_with_tooltip;

// 层级 FFI 的唯一出口：`level.rs` 里全是 `pub(super)`，外界只能经这里的
// 重导出使用。便签（borderless、无图钉控件）用的就是这三个函数。
pub(crate) use level::{is_pinned, move_origin, set_pinned};
