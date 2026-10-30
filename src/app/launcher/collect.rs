//! Launcher items 的收集：遍历模块 → 使用频率排序 → 关键词覆盖。

use gpui_kit::App;

use super::item::LauncherItem;
use crate::app::shell::AppShell;
use crate::app::{keywords, usage};

/// 一次收集的结果：items 与收集时的 revision。
pub(crate) struct CollectedItems {
    pub items: Vec<LauncherItem>,
    pub revision: u64,
}

/// 从所有模块收集 items；同时记下当前的 revision。
///
/// Launcher 打开时收集一次；打开期间若有模块改了内容（例如应用图标生成完
/// 成），模块会把 `AppShell::items_revision` 加一，下一次绘制时这里会被再
/// 调用一次（见 `Launcher::render`）。
pub(crate) fn collect_all(cx: &App) -> CollectedItems {
    let mut items = Vec::new();
    for module in &cx.global::<AppShell>().modules {
        items.extend(module.collect_items(cx));
    }
    // 使用频率排序（宿主公共能力）：按时间衰减分数全局混排，最近常用过的
    // item 排最前（Raycast 风格平铺）。所有模块的 item 自动生效，模块自己
    // 不用做任何事。
    let scores = usage::score_map();
    usage::sort_for_launcher(&mut items, &scores);
    // 用户编辑过的搜索关键词（item-keywords.json）覆盖模块默认值。
    keywords::apply(&mut items, &keywords::path());
    let revision = cx.global::<AppShell>().items_revision.get();
    CollectedItems { items, revision }
}
