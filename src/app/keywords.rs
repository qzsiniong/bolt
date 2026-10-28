//! 用户编辑过的搜索关键词：item id → keywords 覆盖值。
//!
//! 模块 collect item 时会声明一组默认 keywords（应用名、别名、脚本文件名…），
//! 用户可以在 Launcher 的动作面板里改写。没有覆盖条目的 item 用模块默认值；
//! 覆盖条目被删掉（输入留空）= 恢复模块默认。
//!
//! 和 [`super::usage`] 一样是宿主公共能力：按 item id 生效，模块自己不用管。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::App;

use crate::app::launcher::item::LauncherItem;

/// item id → 覆盖后的 keywords。
pub(crate) type KeywordOverrides = HashMap<String, Vec<String>>;

/// `~/Library/Application Support/bolt/item-keywords.json`
pub(crate) fn path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("item-keywords.json");
    path
}

/// 读覆盖表：文件缺失或损坏都当作空表（全部用模块默认）。
pub(crate) fn load(path: &Path) -> KeywordOverrides {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// 把覆盖表套到收集好的 items 上：模块声明的 keywords 让位给用户编辑值。
pub(crate) fn apply(items: &mut [LauncherItem], path: &Path) {
    let overrides = load(path);
    if overrides.is_empty() {
        return;
    }
    for item in items {
        if let Some(keywords) = overrides.get(&item.id) {
            item.keywords = keywords.clone();
        }
    }
}

/// 编辑框文本 → keywords：逗号（中英文）分隔，去首尾空白与空项。
pub(crate) fn parse(text: &str) -> Vec<String> {
    text.split([',', '，'])
        .map(str::trim)
        .filter(|kw| !kw.is_empty())
        .map(str::to_string)
        .collect()
}

/// 保存用户编辑的关键词：空输入 = 删掉覆盖条目（恢复模块默认）；其余按
/// item id 覆盖。落盘失败经 console 告警，不影响当前窗口。
pub(crate) fn set(cx: &App, item_id: &str, text: &str) {
    let keywords = parse(text);
    let path = path();
    let mut overrides = load(&path);
    if keywords.is_empty() {
        overrides.remove(item_id);
    } else {
        overrides.insert(item_id.to_string(), keywords);
    }
    if let Err(e) = write_json(&path, &overrides, "搜索关键词") {
        cx.global::<crate::app::shell::AppShell>()
            .console_channel
            .warn(format!("保存搜索关键词失败：{e}"));
    }
    // 让下一次 collect 拿到新关键词（打开中的 launcher 下一次绘制重收集）。
    let shell = cx.global::<crate::app::shell::AppShell>();
    shell.items_revision.set(shell.items_revision.get() + 1);
}

/// JSON 落盘的公共尾部：建目录 → pretty 序列化 → 写文件。
fn write_json<T: serde::Serialize>(path: &Path, value: &T, what: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(value).map_err(|e| format!("序列化{what}失败：{e}"))?;
    std::fs::write(path, json).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui_kit::App;
    use gpui_kit::assets::IconName;

    use super::*;
    use crate::app::launcher::item::{LauncherIcon, LauncherItem};
    use crate::test_support::TempDir;

    fn item(id: &str) -> LauncherItem {
        LauncherItem {
            id: id.to_string(),
            title: "Safari".to_string(),
            subtitle: None,
            icon: LauncherIcon::Named(IconName::AppWindow),
            group: "test".to_string(),
            keywords: Vec::new(),
            on_confirm: Arc::new(|_: &mut App| {}),
            actions: Vec::new(),
            paste: None,
        }
    }

    #[test]
    fn parse_splits_on_both_commas_and_trims() {
        assert_eq!(parse("微信, wechat，pin"), ["微信", "wechat", "pin"]);
        assert!(parse("  , ， ").is_empty());
    }

    #[test]
    fn overrides_replace_the_module_defaults() {
        let temp = TempDir::new("keywords");
        let path = temp.0.join("item-keywords.json");
        let overrides: KeywordOverrides = [
            ("apps.open:safari".to_string(), vec!["safari".to_string()]),
            (
                "apps.open:arc".to_string(),
                vec!["arc".to_string(), "浏览器".to_string()],
            ),
        ]
        .into_iter()
        .collect();
        write_json(&path, &overrides, "搜索关键词").expect("write");

        let mut items = vec![item("apps.open:safari"), item("apps.open:zed")];
        apply(&mut items, &path);
        assert_eq!(items[0].keywords, ["safari"]);
        // 没有覆盖条目的 item 保持模块默认（这里是空）。
        assert!(items[1].keywords.is_empty());
    }

    #[test]
    fn missing_file_means_module_defaults() {
        let temp = TempDir::new("keywords-missing");
        let mut items = vec![item("apps.open:safari")];
        apply(&mut items, &temp.0.join("nope.json"));
        assert!(items[0].keywords.is_empty());
    }
}
