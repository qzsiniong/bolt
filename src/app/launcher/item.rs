//! Launcher 与内容模块之间的契约：[`LauncherItem`] 是列表里的一行，
//! [`Module`] 是模块接入 Launcher 的方式。
//!
//! 依赖方向：`modules → app::launcher`（模块实现 trait）；本文件不 import
//! 任何具体模块，新增模块对 Launcher 零改动。

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::ImageSource;
use gpui_kit::assets::IconName;

/// item 点击后的执行逻辑。
///
/// 用 `Arc + Send + Sync` 是因为确认请求要经 crossbeam channel 传到 app
/// tick 执行：confirm 回调里 launcher 窗口已经关了，无法回调 launcher
/// 实体，闭包必须脱离它独立存活。实际只在主线程被调用；闭包只捕获
/// `PathBuf` / `String` / 扩展数据等 `Send + Sync` 值， bound 是诚实的。
pub type LauncherAction = Arc<dyn Fn(&mut App) + Send + Sync>;

/// 列表行左侧的图标：内置图标，或应用自己的内存图片。
///
/// 应用启动器需要每个 App 的真实图标，`IconName` 是固定枚举表达不了；
/// 图片走 `ImageSource::Image`（内存 PNG 字节），不能用 `data:` URI
/// ——那个字符串会被 gpui 当成 URI 去 fetch 资源，取不到就静默空白。
#[derive(Clone)]
pub enum LauncherIcon {
    Named(IconName),
    Image(ImageSource),
}

impl From<IconName> for LauncherIcon {
    fn from(icon: IconName) -> Self {
        Self::Named(icon)
    }
}

impl From<ImageSource> for LauncherIcon {
    fn from(source: ImageSource) -> Self {
        Self::Image(source)
    }
}

/// Launcher 列表里的一行。模块在 collect 时把执行所需数据（路径、扩展
/// 对象等）捕获进 [`Self::on_confirm`] 闭包，Launcher 只负责展示与触发，
/// 不知道 item 属于哪个模块。
#[derive(Clone)]
pub struct LauncherItem {
    /// 全局唯一 id：`<module>.<local>`，如 `scripts.open-manager`、
    /// `scripts.run:foo.js`、`json_editor.open`、`ext.demo.go`、`ui.ui-demo`。
    /// id 同时参与搜索。
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub icon: LauncherIcon,
    /// 分组标题（"内置" / "脚本" / 扩展 display_name）。不同模块的同名
    /// 分组合并；组的位置按首次出现顺序。
    pub group: String,
    /// 点击执行逻辑。
    pub on_confirm: LauncherAction,
}

/// 一个内容模块：为 Launcher 提供 items。
pub trait Module: 'static {
    /// 模块名（日志/调试用）。
    fn name(&self) -> &'static str;

    /// 收集该模块贡献的 items。每次 Launcher 打开都重新调用（磁盘内容
    /// 可能变了）。错误由模块自己经 console 上报，不影响其他模块。
    fn collect_items(&self, cx: &App) -> Vec<LauncherItem>;
}
