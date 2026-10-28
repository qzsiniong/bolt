//! 内置的 App 搜索别名默认表：按 bundle identifier 补关键词。
//!
//! 很多 App 的 Info.plist / URL schemes 里并没有用户常用来搜它的名字
//! （例如企业微信 bundle 里完全没有 `weixin`），所以这里内置一份默认表。
//!
//! 用户自定义搜索词**不在这份文件里**：统一走 Launcher 的「编辑搜索关键词」
//! 动作（存 `~/Library/Application Support/bolt/item-keywords.json`，按
//! item id 覆盖整个 keywords）。历史上曾把默认表播种到
//! `app-aliases.json` 供手动编辑，现已被 UI 编辑取代，不再读写磁盘；
//! 旧文件留在原处不影响任何行为。
//!
//! 默认值写在同目录的 `default-aliases.json`（单独一份文件方便维护和
//! JSON 语法高亮），用 `include_str!` 编进二进制——缺文件或 JSON 格式错
//! 会在编译 / 启动时立刻报错，不会静默退到空表。

use std::collections::HashMap;

/// 别名表：bundle identifier → 额外关键词数组。
///
/// HashMap 保证 O(1) 查找；Vec 保持 JSON 里书写的顺序（对调试友好）。
pub(crate) type AliasMap = HashMap<String, Vec<String>>;

/// 内置默认别名表（编译期嵌入，不做任何磁盘 IO）。
pub(crate) fn default_aliases() -> AliasMap {
    const DEFAULT_JSON: &str = include_str!("default-aliases.json");
    serde_json::from_str::<AliasMap>(DEFAULT_JSON)
        .expect("default-aliases.json 解析失败：请检查 JSON 语法")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_aliases_cover_wecom_weixin() {
        let d = default_aliases();
        assert!(
            d.get("com.tencent.WeWorkMac")
                .unwrap()
                .iter()
                .any(|a| a == "weixin")
        );
        assert!(
            d.get("com.tencent.xinWeChat")
                .unwrap()
                .iter()
                .any(|a| a == "微信")
        );
    }
}
