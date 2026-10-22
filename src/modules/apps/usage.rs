//! 应用使用频率：记录启动次数与最近使用时间，落盘到应用数据目录，
//! 用来把常用应用排在列表前面（Raycast 的行为）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::scan::AppEntry;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub(crate) struct UsageEntry {
    #[serde(default)]
    pub(crate) count: u32,
    /// 最近一次启动的 Unix 秒。
    #[serde(default)]
    pub(crate) last_used: u64,
}

pub(crate) type UsageMap = HashMap<String, UsageEntry>;

/// `~/Library/Application Support/bolt/apps-usage.json`
pub(crate) fn usage_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("apps-usage.json");
    path
}

/// 读盘：文件缺失或内容损坏都当作空表（不能因为统计文件坏了就打不开列表）。
pub(crate) fn load(path: &Path) -> UsageMap {
    let Ok(text) = std::fs::read_to_string(path) else {
        return UsageMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
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

pub(crate) fn key(app: &AppEntry) -> String {
    app.path.to_string_lossy().into_owned()
}

pub(crate) fn bump(usage: &mut UsageMap, key: &str) {
    let entry = usage.entry(key.to_string()).or_default();
    entry.count = entry.count.saturating_add(1);
    entry.last_used = now_secs();
}

/// 排序：次数降序 → 最近使用降序 → 名称升序（保证同一毫秒内也稳定）。
pub(crate) fn sort_by_usage(apps: &mut [AppEntry], usage: &UsageMap) {
    let rank = |app: &AppEntry| {
        usage
            .get(&key(app))
            .map(|e| (e.count, e.last_used))
            .unwrap_or((0, 0))
    };
    apps.sort_by(|a, b| {
        rank(b)
            .cmp(&rank(a))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
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
    use super::*;
    use crate::test_support::TempDir;

    fn entry(name: &str) -> AppEntry {
        AppEntry {
            path: PathBuf::from(format!("/Applications/{name}.app")),
            name: name.to_string(),
            original_name: name.to_string(),
            subtitle: "应用".to_string(),
            keywords: Vec::new(),
        }
    }

    #[test]
    fn roundtrips_and_tolerates_a_broken_file() {
        let temp = TempDir::new("apps-usage");
        let path = temp.0.join("apps-usage.json");

        let mut usage = UsageMap::new();
        bump(&mut usage, "/Applications/Safari.app");
        save(&path, &usage).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded["/Applications/Safari.app"].count, 1);

        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load(&path).is_empty());
        assert!(load(&temp.0.join("missing.json")).is_empty());
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
    fn sorts_by_count_then_recency_then_name() {
        let mut apps = vec![entry("Zed"), entry("Chrome"), entry("Safari"), entry("Arc")];
        let mut usage = UsageMap::new();
        usage.insert(
            "/Applications/Safari.app".into(),
            UsageEntry {
                count: 3,
                last_used: 10,
            },
        );
        usage.insert(
            "/Applications/Chrome.app".into(),
            UsageEntry {
                count: 3,
                last_used: 20,
            },
        );
        usage.insert(
            "/Applications/Arc.app".into(),
            UsageEntry {
                count: 1,
                last_used: 99,
            },
        );
        sort_by_usage(&mut apps, &usage);
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Chrome", "Safari", "Arc", "Zed"]);
    }
}
