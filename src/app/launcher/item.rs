//! Launcher 与内容模块之间的契约：[`LauncherItem`] 是列表里的一行，
//! [`Module`] 是模块接入 Launcher 的方式。
//!
//! 依赖方向：`modules → app::launcher`（模块实现 trait）；本文件不 import
//! 任何具体模块，新增模块对 Launcher 零改动。

use std::sync::Arc;

use gpui_kit::App;
use gpui_kit::ImageSource;
use gpui_kit::assets::IconName;

use crate::ui::clipboard::PasteContent;

/// item 点击后的执行逻辑。
///
/// 用 `Arc + Send + Sync` 是因为确认请求要经 crossbeam channel 传到 app
/// tick 执行：confirm 回调里 launcher 窗口已经关了，无法回调 launcher
/// 实体，闭包必须脱离它独立存活。实际只在主线程被调用；闭包只捕获
/// `PathBuf` / `String` / 扩展数据等 `Send + Sync` 值， bound 是诚实的。
pub type LauncherAction = Arc<dyn Fn(&mut App) + Send + Sync>;

/// 粘贴内容的处理逻辑：确认时经同一个 channel 送出（同 [`LauncherAction`]
/// 的存活约束），内容本体 move 进闭包。
pub type PasteAction = Arc<dyn Fn(&mut App, &PasteContent) + Send + Sync>;

/// 声明 item 能处理哪种粘贴内容（Launcher 的 Cmd+V 识别）。
///
/// `accepts` 是无状态的 fn 指针（声明式、可 Copy）；模块只对自己能处理的
/// 变体返回 true。新增内容类型 = `PasteContent` 加变体 + 目标模块改这里。
/// token 徽章显示的类型名由内容本身决定（`PasteContent::kind_label`）。
#[derive(Clone)]
pub struct PasteHandler {
    pub accepts: fn(&PasteContent) -> bool,
    pub run: PasteAction,
}

/// Cmd+K 动作面板里的一个动作：标题 + 可选快捷键提示 + 执行闭包。
///
/// `shortcut` 只是展示文本（如 "↵"），不绑定真实按键——面板打开时 Enter
/// 始终执行当前选中的动作。模块别放没接线的提示，会误导用户。
///
/// `dialog` 为 Some 时表示「这个 action 只需要弹 dialog 展示内容」，
/// Launcher 收到后直接 open AlertDialog、不走 channel，也不关窗口。
/// 用于展示类动作（如显示搜索关键词），避免经 Console 绕路。
#[derive(Clone)]
pub struct ItemAction {
    pub title: String,
    pub shortcut: Option<&'static str>,
    pub run: LauncherAction,
    pub dialog: Option<DialogSpec>,
}

/// AlertDialog 展示类动作的配置。所有字段都带默认值：调用方只写 title
/// 和 description 就够了，ok_text 不传默认"关闭"，show_cancel 默认 false。
#[derive(Clone)]
pub struct DialogSpec {
    pub title: String,
    pub description: String,
    pub ok_text: Option<String>,
    pub show_cancel: bool,
}

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
    /// 额外的搜索关键词（除了 id、title、subtitle 之外的别名）。例如应用
    /// 会把原始文件名、bundle id 和 URL schemes 放进来，让「微信」、
    /// wechat、weixin 都能搜到同一个 App。
    pub keywords: Vec<String>,
    /// 点击执行逻辑。
    pub on_confirm: LauncherAction,
    /// Cmd+K 动作面板的动作列表。为空时 Launcher 回退成单个「打开」
    /// （执行 [`Self::on_confirm`]），模块不用为普通 item 手写。
    pub actions: Vec<ItemAction>,
    /// 能处理的粘贴内容（Cmd+V 识别后按此过滤）。None = 与粘贴无关，
    /// 不参与粘贴过滤。声明后回车确认时内容会传给 handler 的 `run`。
    pub paste: Option<PasteHandler>,
}

/// 一个内容模块：为 Launcher 提供 items。
pub trait Module: 'static {
    /// 模块名（日志/调试用）。
    fn name(&self) -> &'static str;

    /// 收集该模块贡献的 items。每次 Launcher 打开都重新调用（磁盘内容
    /// 可能变了）。错误由模块自己经 console 上报，不影响其他模块。
    fn collect_items(&self, cx: &App) -> Vec<LauncherItem>;
}
