//! 应用启动器模块：把 `/Applications` 等目录下的 `.app` 列进 Launcher，
//! 点击直接启动。图标是应用的真实图标（`NSWorkspace` 生成并缓存到数据
//! 目录），按使用次数/最近使用排序，贴近 Raycast 的用法。

pub(crate) mod aliases;
pub(crate) mod icons;
pub(crate) mod names;
pub(crate) mod scan;
pub(crate) mod usage;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::{App, Image, ImageFormat, ImageSource};

use crate::app::launcher::{ItemAction, LauncherAction, LauncherIcon, LauncherItem, Module};
use crate::app::shell::AppShell;
use crate::app::windows;
use crate::ui::mac_ffi;
use scan::AppEntry;
use usage::UsageMap;

/// 启动后先等一会儿再补图标：让应用完成启动、全局快捷键的事件 tap 挂上
/// 主 run loop，再开始 AppKit 调用。
const ICON_FILL_START_DELAY: Duration = Duration::from_millis(400);

/// 每批最多占用主线程多久。`NSWorkspace` 只能在主线程调用，所以只能按时间
/// 切片，绝对不能一口气遍历上百个应用——主线程被长时间占住会让全局快捷键
/// 的事件 tap 超时，Launcher 就按不出来了（见 [`super::icons`] 的模块注释）。
const ICON_FILL_BUDGET: Duration = Duration::from_millis(6);

/// 两批之间让出主线程多久。只要每次阻塞不超过一帧，事件循环就能正常处理
/// 热键与窗口绘制。
const ICON_FILL_PAUSE: Duration = Duration::from_millis(4);

pub(crate) struct AppsModule {
    data: Arc<AppsData>,
    /// 启动后补齐缺失图标的后台任务；常驻模块期间必须持有，否则会被取消。
    _icon_task: gpui_kit::Task<()>,
}

struct AppsData {
    apps: Vec<AppEntry>,
    usage: Mutex<UsageMap>,
    /// 应用路径 → 已缓存的 PNG 字节。
    icons: Mutex<HashMap<PathBuf, Vec<u8>>>,
}

impl AppsModule {
    pub(crate) fn new(cx: &mut App) -> Self {
        let mut apps = scan::scan_apps(&scan::default_roots());
        // 本地化显示名（WeChat → 微信）+ bundle id / URL schemes 作为搜索
        // 关键词，让用户可以用「微信」、wechat、weixin 搜到同一个 App。
        names::fill_display_names_and_keywords(&mut apps);
        let usage = usage::load(&usage::usage_path());
        // 磁盘缓存一次读完：每个图标只有几 KB，比逐个异步读简单。
        let mut icons = HashMap::new();
        for app in &apps {
            if let Some(bytes) = icons::load_cached(&app.path) {
                icons.insert(app.path.clone(), bytes);
            }
        }

        let data = Arc::new(AppsData {
            apps,
            usage: Mutex::new(usage),
            icons: Mutex::new(icons),
        });

        // 缺失的图标在主线程分批补齐（`NSWorkspace` 只能在主线程调用）。
        // 单个图标已是毫秒级（见 `icons` 模块注释），但整批仍按时间切片并
        // 在批间让出主线程：首次启动生成上百个图标时，热键、窗口和绘制都
        // 不能因此卡住。
        let fill = data.clone();
        let icon_task = cx.spawn(async move |cx| {
            let pending: Vec<PathBuf> = {
                let icons = lock(&fill.icons);
                fill.apps
                    .iter()
                    .filter(|app| !icons.contains_key(&app.path))
                    .map(|app| app.path.clone())
                    .collect()
            };
            if pending.is_empty() {
                return;
            }

            cx.background_executor().timer(ICON_FILL_START_DELAY).await;

            let total = pending.len();
            let mut failed = 0usize;
            let mut first_error = String::new();
            let mut index = 0;
            while index < pending.len() {
                let batch_deadline = Instant::now() + ICON_FILL_BUDGET;
                while index < pending.len() && Instant::now() < batch_deadline {
                    let path = &pending[index];
                    index += 1;
                    match icons::icon_bytes(path) {
                        Ok(png) => {
                            lock(&fill.icons).insert(path.clone(), png);
                        }
                        Err(e) => {
                            failed += 1;
                            if first_error.is_empty() {
                                first_error = format!("{}：{e}", path.display());
                            }
                        }
                    }
                }
                cx.background_executor().timer(ICON_FILL_PAUSE).await;
            }

            let summary = if failed == 0 {
                format!("应用图标：已生成 {total} 个新图标")
            } else {
                format!(
                    "应用图标：生成 {} 个，失败 {failed} 个（{first_error}）",
                    total - failed
                )
            };
            cx.update(|cx| {
                let console = &cx.global::<AppShell>().console_channel;
                if failed == 0 {
                    console.info(summary);
                } else {
                    console.warn(summary);
                }
            });
            // 图标可能是在 Launcher 已经打开期间补齐的：让它在下一帧重新
            // 收集 items，已经打开的命令面板不用等下次唤起才显示图标。
            cx.update(windows::items_changed);
        });

        Self {
            data,
            _icon_task: icon_task,
        }
    }
}

impl Module for AppsModule {
    fn name(&self) -> &'static str {
        "apps"
    }

    fn collect_items(&self, _cx: &App) -> Vec<LauncherItem> {
        items_from(&self.data)
    }
}

/// 纯函数：按使用频率排序后构造 items（与 App 上下文解耦，方便单测）。
fn items_from(data: &Arc<AppsData>) -> Vec<LauncherItem> {
    let mut apps = data.apps.clone();
    {
        let usage = lock(&data.usage);
        usage::sort_by_usage(&mut apps, &usage);
    }
    let icons = lock(&data.icons);
    apps.into_iter()
        .map(|app| item_for(app, data, &icons))
        .collect()
}

fn item_for(
    app: AppEntry,
    data: &Arc<AppsData>,
    icons: &HashMap<PathBuf, Vec<u8>>,
) -> LauncherItem {
    let icon = match icons.get(&app.path) {
        Some(bytes) => LauncherIcon::Image(ImageSource::Image(Arc::new(Image::from_bytes(
            ImageFormat::Png,
            bytes.clone(),
        )))),
        // 图标还没生成好（首次启动的头几秒）：先用内置图标占位。
        None => LauncherIcon::Named(IconName::AppWindow),
    };

    let path = app.path;
    let name = app.name;
    let data = data.clone();

    // 搜索关键词：title 本地化后可能是中文（微信），但用户也可能输入
    // wechat / weixin。所以把 original_name、bundle id、URL schemes 都
    // 加进来。Command 组件还会额外匹配 label（title）、id、subtitle，
    // 所以这些 keywords 是「额外」补充。
    let mut keywords = vec![app.original_name];
    keywords.extend(app.keywords);

    let primary: LauncherAction = Arc::new({
        let data = data.clone();
        let path = path.clone();
        let name = name.clone();
        move |cx: &mut App| launch(cx, data.as_ref(), &path, &name)
    });
    // Cmd+K 动作：打开（主动作，带使用统计）/ 在 Finder 中显示 / 复制路径。
    let reveal_path = path.clone();
    let copy_path = path.to_string_lossy().into_owned();
    let actions = vec![
        ItemAction {
            title: "打开应用".to_string(),
            shortcut: Some("↵"),
            run: primary.clone(),
            dialog: None,
        },
        ItemAction {
            title: "在 Finder 中显示".to_string(),
            shortcut: None,
            run: Arc::new(move |_| match mac_ffi::reveal_in_finder(&reveal_path) {
                Ok(()) => mac_ffi::notify(
                    "Bolt",
                    &format!(
                        "已在 Finder 中显示 {}",
                        reveal_path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ),
                ),
                Err(e) => mac_ffi::notify("Bolt", &format!("显示失败：{e}")),
            }),
            dialog: None,
        },
        ItemAction {
            title: "复制路径".to_string(),
            shortcut: None,
            run: Arc::new(move |_| match mac_ffi::copy_text(&copy_path) {
                Ok(()) => mac_ffi::notify("Bolt", "路径已复制到剪贴板"),
                Err(e) => mac_ffi::notify("Bolt", &format!("复制失败：{e}")),
            }),
            dialog: None,
        },
    ];

    LauncherItem {
        id: format!("apps.open:{}", path.display()),
        title: name.clone(),
        subtitle: Some(app.subtitle),
        icon,
        group: "应用".to_string(),
        keywords,
        on_confirm: primary,
        actions,
    }
}

/// 记录一次使用并启动应用。
fn launch(cx: &mut App, data: &AppsData, path: &Path, name: &str) {
    let console = cx.global::<AppShell>().console_channel.clone();

    {
        let mut usage = lock(&data.usage);
        usage::bump(&mut usage, &path.to_string_lossy());
        if let Err(e) = usage::save(&usage::usage_path(), &usage) {
            console.warn(format!("保存应用使用记录失败：{e}"));
        }
    }

    match std::process::Command::new("open").arg(path).status() {
        Ok(status) if status.success() => console.info(format!("已启动应用：{name}")),
        Ok(status) => console.warn(format!(
            "启动应用 {name} 失败：open 退出码 {:?}",
            status.code()
        )),
        Err(e) => console.error(format!("启动应用 {name} 失败：{e}")),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> AppEntry {
        AppEntry {
            path: PathBuf::from(format!("/Applications/{name}.app")),
            name: name.to_string(),
            original_name: name.to_string(),
            subtitle: "应用".to_string(),
            keywords: Vec::new(),
        }
    }

    fn data_with(
        apps: Vec<AppEntry>,
        usage: UsageMap,
        icons: HashMap<PathBuf, Vec<u8>>,
    ) -> Arc<AppsData> {
        Arc::new(AppsData {
            apps,
            usage: Mutex::new(usage),
            icons: Mutex::new(icons),
        })
    }

    #[test]
    fn sorts_by_usage_and_falls_back_to_a_named_icon() {
        let mut usage = UsageMap::new();
        usage::bump(&mut usage, "/Applications/Zed.app");
        let icons = HashMap::from([(PathBuf::from("/Applications/Safari.app"), vec![1, 2, 3])]);
        let data = data_with(vec![app("Safari"), app("Zed")], usage, icons);

        let items = items_from(&data);
        assert_eq!(items[0].id, "apps.open:/Applications/Zed.app");
        assert_eq!(items[1].id, "apps.open:/Applications/Safari.app");
        assert!(items.iter().all(|item| item.group == "应用"));
        // 有缓存字节的用真实图标，没有的退回内置图标。
        assert!(matches!(items[0].icon, LauncherIcon::Named(_)));
        assert!(matches!(items[1].icon, LauncherIcon::Image(_)));
    }
}
