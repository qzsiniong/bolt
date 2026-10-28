//! Launcher item 使用频率：记录每次确认的时间事件，按「时间衰减 + 使用次数」
//! 打分排序（Raycast 的行为），落盘到应用数据目录。
//!
//! 这是宿主公共能力：任何模块的 item 只要带唯一 `id`（如 `apps.open:<path>`、
//! `scripts.run:<file>`）就自动获得频率统计与排序，模块自己不用做任何事。
//! 计数点在 launcher 的主回车确认处（`Launcher::on_confirm`），排序点在
//! `collect_all` 收集完所有模块 items 之后。
//!
//! 打分本身由 [`crate::core::smart_rank`] 负责；这里只决定「存哪个文件、分数
//! 怎么落成 items 顺序」。搜索关键词覆盖是另一件事，见 [`super::keywords`]。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::App;

use crate::app::launcher::item::LauncherItem;
use crate::core::file_storage::FileStorage;
use crate::core::smart_rank::{RankConfig, SmartRank};

/// `~/Library/Application Support/bolt/item-usage.json`
pub(crate) fn usage_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("item-usage.json");
    path
}

/// 一张记录文件对应的排序引擎。每次操作现建：读一次文件即可，不持有跨调用
/// 状态（和 launcher「打开时收集一次、确认时记一次」的节奏一致）。
fn engine(path: &Path) -> SmartRank {
    SmartRank::new(RankConfig::default(), Box::new(FileStorage::new(path)))
}

/// 所有 item 的当前分数。读失败或数据损坏时退化成空表（纯按标题排序），
/// 不能因为统计文件坏了就打不开列表。
pub(crate) fn score_map() -> HashMap<String, f64> {
    engine(&usage_path()).scores().unwrap_or_default()
}

/// 确认一次 item（launcher 主回车）：记一个「现在」的事件；失败只告警，
/// 不影响已经确认的动作。
pub(crate) fn record_confirm(cx: &App, item_id: &str) {
    if let Err(e) = engine(&usage_path()).record(item_id) {
        cx.global::<crate::app::shell::AppShell>()
            .console_channel
            .warn(format!("保存使用记录失败：{e}"));
    }
}

/// 启动时做一次全量裁剪：删掉超过保留期的老事件，避免记录文件无限增长
/// （`record` 只裁被记录的那个 item）。失败静默：统计文件不是关键路径。
pub(crate) fn prune_usage() {
    let _ = engine(&usage_path()).prune();
}

/// 排序（全局混排，Raycast 风格平铺）：衰减分数降序 → title 升序。
///
/// 没有使用记录（或事件都落在评分窗口外）的 item 分数为 0，彼此按 title 排，
/// 与文件名扫描序基本一致。
pub(crate) fn sort_for_launcher(items: &mut [LauncherItem], scores: &HashMap<String, f64>) {
    items.sort_by(|left, right| {
        let left_score = scores.get(&left.id).copied().unwrap_or(0.0);
        let right_score = scores.get(&right.id).copied().unwrap_or(0.0);
        right_score
            .total_cmp(&left_score)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::App;
    use gpui_kit::assets::IconName;

    use super::*;
    use crate::app::launcher::item::{LauncherIcon, LauncherItem};
    use crate::test_support::TempDir;

    fn item(id: &str, title: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: title.to_string(),
            subtitle: None,
            icon: LauncherIcon::Named(IconName::AppWindow),
            group: "test".to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|_: &mut App| {}),
            actions: Vec::new(),
            paste: None,
        }
    }

    fn ids(items: &[LauncherItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn sorts_by_score_then_title() {
        let mut items = vec![
            item("apps.open:zed", "Zed"),
            item("scripts.run:b", "b"),
            item("apps.open:safari", "Safari"),
            item("apps.open:arc", "Arc"),
            item("scripts.run:a", "a"),
        ];
        let scores: HashMap<String, f64> = [
            ("apps.open:safari".to_string(), 2.0),
            ("apps.open:arc".to_string(), 1.0),
        ]
        .into_iter()
        .collect();

        sort_for_launcher(&mut items, &scores);

        // 全局混排（平铺）：有分数的按分数排最前；没有记录的彼此按 title 排。
        assert_eq!(
            ids(&items),
            [
                "apps.open:safari",
                "apps.open:arc",
                "scripts.run:a",
                "scripts.run:b",
                "apps.open:zed",
            ]
        );
    }

    #[test]
    fn recorded_events_rank_before_unused_items() {
        let temp = TempDir::new("usage-rank");
        let path = temp.0.join("item-usage.json");
        let engine = engine(&path);
        engine.record("apps.open:arc").expect("record");
        engine.record("apps.open:arc").expect("record");
        engine.record("apps.open:zed").expect("record");
        let scores = engine.scores().expect("scores");

        let mut items = vec![
            item("apps.open:zed", "Zed"),
            item("scripts.run:a", "a"),
            item("apps.open:arc", "Arc"),
        ];
        sort_for_launcher(&mut items, &scores);
        assert_eq!(
            ids(&items),
            ["apps.open:arc", "apps.open:zed", "scripts.run:a"]
        );
    }

    #[test]
    fn unreadable_history_does_not_block_recording() {
        let temp = TempDir::new("usage-fresh-start");
        let path = temp.0.join("item-usage.json");
        // 读不懂的历史（老格式 / 损坏文件）：记录从空表重新开始。
        std::fs::write(&path, br#"{ "apps.open:x": { "count": 3 } }"#).expect("write");

        let engine = engine(&path);
        assert!(engine.scores().is_err(), "读不懂的数据应当报错");
        engine
            .record("apps.open:arc")
            .expect("record should recover");

        let scores = engine.scores().expect("scores after reset");
        assert!(scores["apps.open:arc"] > 0.0);
        assert!(!scores.contains_key("apps.open:x"));
    }
}
