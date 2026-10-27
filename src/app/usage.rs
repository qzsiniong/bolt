//! Launcher item 使用频率：记录确认次数与最近使用时间，落盘到应用数据目录，
//! 用来把常用 item 排在所属分组前面（Raycast 的行为）。
//!
//! 这是宿主公共能力：任何模块的 item 只要带唯一 `id`（如 `apps.open:<path>`、
//! `scripts.run:<file>`）就自动获得频率统计与排序，模块自己不用做任何事。
//! 计数点在 launcher 的主回车确认处（`Launcher::on_confirm`），排序点在
//! `collect_all` 收集完所有模块 items 之后。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use gpui_kit::App;

use crate::app::launcher::item::LauncherItem;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub(crate) struct UsageEntry {
    #[serde(default)]
    pub(crate) count: u32,
    /// 最近一次使用的 Unix 秒。
    #[serde(default)]
    pub(crate) last_used: u64,
}

pub(crate) type UsageMap = HashMap<String, UsageEntry>;

/// `~/Library/Application Support/bolt/item-usage.json`
pub(crate) fn usage_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("item-usage.json");
    path
}

/// 读盘：文件缺失或内容损坏都当作空表（不能因为统计文件坏了就打不开列表）。
///
/// 兼容迁移：apps 模块时代的统计在 `apps-usage.json`，key 是裸 .app 路径；
/// 首次读取（新文件还不存在）时搬过来并统一加上 item.id 的 `apps.open:` 前缀。
/// 不主动回写——下次 bump 落盘时自然写进新文件。
pub(crate) fn load(path: &Path) -> UsageMap {
    if let Ok(text) = std::fs::read_to_string(path) {
        return serde_json::from_str(&text).unwrap_or_default();
    }
    let mut legacy = path.to_path_buf();
    legacy.set_file_name("apps-usage.json");
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return UsageMap::new();
    };
    serde_json::from_str::<UsageMap>(&text)
        .unwrap_or_default()
        .into_iter()
        .map(|(key, entry)| (format!("apps.open:{key}"), entry))
        .collect()
}

pub(crate) fn save(path: &Path, usage: &UsageMap) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
    }
    let json =
        serde_json::to_string_pretty(usage).map_err(|e| format!("序列化使用记录失败：{e}"))?;
    std::fs::write(path, json).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 记一次使用：count +1 并刷新最近使用时间。
pub(crate) fn bump(usage: &mut UsageMap, key: &str) {
    let entry = usage.entry(key.to_string()).or_default();
    entry.count = entry.count.saturating_add(1);
    entry.last_used = now_secs();
}

/// 确认一次 item（launcher 主回车）：bump + 落盘，失败只告警不影响启动。
pub(crate) fn record_confirm(cx: &App, item_id: &str) {
    let mut usage = load(&usage_path());
    bump(&mut usage, item_id);
    if let Err(e) = save(&usage_path(), &usage) {
        cx.global::<crate::app::shell::AppShell>()
            .console_channel
            .warn(format!("保存使用记录失败：{e}"));
    }
}

/// 排序（全局混排，Raycast 风格平铺）：使用次数降序 → 最近使用降序 →
/// title 升序（沿用 apps 时代的三键规则；从未用过的 item 彼此按 title 排，
/// 与文件名扫描序基本一致）。
pub(crate) fn sort_for_launcher(items: &mut [LauncherItem], usage: &UsageMap) {
    let rank = |item: &LauncherItem| {
        usage
            .get(&item.id)
            .map(|e| (e.count, e.last_used))
            .unwrap_or((0, 0))
    };
    items.sort_by(|a, b| {
        rank(b)
            .cmp(&rank(a))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::App;
    use gpui_kit::assets::IconName;

    use super::*;
    use crate::app::launcher::item::{LauncherIcon, LauncherItem};
    use crate::test_support::TempDir;

    fn item(id: &str, group: &str, title: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: title.to_string(),
            subtitle: None,
            icon: LauncherIcon::Named(IconName::AppWindow),
            group: group.to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|_: &mut App| {}),
            actions: Vec::new(),
        }
    }

    #[test]
    fn roundtrips_and_tolerates_a_broken_file() {
        let temp = TempDir::new("item-usage-rw");
        let path = temp.0.join("item-usage.json");

        let mut usage = UsageMap::new();
        bump(&mut usage, "apps.open:/Applications/Safari.app");
        save(&path, &usage).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded["apps.open:/Applications/Safari.app"].count, 1);

        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load(&path).is_empty());
        assert!(load(&temp.0.join("missing.json")).is_empty());
    }

    #[test]
    fn migrates_legacy_apps_usage_by_prefixing_the_id() {
        let temp = TempDir::new("item-usage-migrate");
        // 旧文件：key 是裸 .app 路径。
        let legacy = temp.0.join("apps-usage.json");
        let mut old = UsageMap::new();
        bump(&mut old, "/Applications/Safari.app");
        save(&legacy, &old).unwrap();

        let loaded = load(&temp.0.join("item-usage.json"));
        assert_eq!(loaded["apps.open:/Applications/Safari.app"].count, 1);
    }

    #[test]
    fn bump_increases_count_and_updates_last_used() {
        let mut usage = UsageMap::new();
        bump(&mut usage, "a");
        let first = usage["a"];
        bump(&mut usage, "a");
        assert_eq!(usage["a"].count, 2);
        assert!(usage["a"].last_used >= first.last_used);
    }

    #[test]
    fn sorts_globally_by_usage_then_title() {
        let mut items = vec![
            item("apps.open:zed", "应用", "Zed"),
            item("scripts.run:b", "脚本", "b"),
            item("apps.open:safari", "应用", "Safari"),
            item("apps.open:arc", "应用", "Arc"),
            item("scripts.run:a", "脚本", "a"),
        ];
        let mut usage = UsageMap::new();
        usage.insert(
            "apps.open:safari".into(),
            UsageEntry {
                count: 2,
                last_used: 10,
            },
        );
        usage.insert(
            "apps.open:arc".into(),
            UsageEntry {
                count: 1,
                last_used: 99,
            },
        );
        sort_for_launcher(&mut items, &usage);

        // 全局混排（平铺）：用过的按次数排最前；没用过的彼此按 title 排。
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "apps.open:safari",
                "apps.open:arc",
                "scripts.run:a",
                "scripts.run:b",
                "apps.open:zed"
            ]
        );
    }
}
