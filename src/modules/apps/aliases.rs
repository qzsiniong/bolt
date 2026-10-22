//! 应用搜索别名配置：按 bundle identifier 挂额外关键词。
//!
//! 很多 App 的 Info.plist / URL schemes 里并没有用户常用来搜它的名字
//! （例如企业微信 bundle 里完全没有 `weixin`），所以这里用一份可编辑的
//! JSON 配置补。配置文件放在：
//!
//!     `~/Library/Application Support/bolt/app-aliases.json`
//!
//! 格式：`{ "bundle.id": ["alias1", "alias2", ...], ... }`。
//!
//! 文件不存在时会自动生成一份默认表，用户可以直接编辑；文件损坏或解析
//! 失败时静默退回内置默认表，不会阻塞启动。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 别名表：bundle identifier → 额外关键词数组。
///
/// HashMap 保证 O(1) 查找；Vec 保持 JSON 里书写的顺序（对调试友好）。
pub(crate) type AliasMap = HashMap<String, Vec<String>>;

/// 配置文件路径，和 apps-usage.json / app-icons 共用 bolt 数据目录。
pub(crate) fn aliases_path() -> PathBuf {
    let mut path = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("bolt");
    path.push("app-aliases.json");
    path
}

/// 从磁盘加载别名表：文件不存在 / 损坏 / 解析失败都返回默认表。
///
/// 文件不存在时会顺手把默认表写出去（让用户拿到一份可直接编辑的模板）。
pub(crate) fn load() -> AliasMap {
    let path = aliases_path();
    if !path.exists() {
        let defaults = default_aliases();
        // 首次启动自动播种，用户打开就能直接编辑。
        let _ = save(&path, &defaults);
        return defaults;
    }
    match read_json(&path) {
        Ok(map) => map,
        Err(e) => {
            eprintln!(
                "[apps] 读取别名表 {} 失败：{e}，使用内置默认表",
                path.display()
            );
            default_aliases()
        }
    }
}

/// 保存别名表到磁盘，自动创建父目录。
pub(crate) fn save(path: &Path, map: &AliasMap) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("创建目录 {} 失败：{e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(map).map_err(|e| format!("序列化别名表失败：{e}"))?;
    std::fs::write(path, json).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

fn read_json(path: &Path) -> Result<AliasMap, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    // serde_json 会识别 `//` 和 `#` 行注释吗？不会。但我们播种的默认 JSON
    // 里没有注释——JSON 标准也不允许。如果未来要加注释，切换到 json5 依赖。
    serde_json::from_str::<AliasMap>(&text)
        .map_err(|e| format!("解析 {} 失败：{e}", path.display()))
}

/// 首次启动播种 + 磁盘解析失败时回退的默认表。
///
/// 默认值写在同目录的 `default-aliases.json` 里（单独一份文件方便维护和
/// JSON 语法高亮），用 `include_str!` 编进二进制——缺文件或 JSON 格式错会
/// 在编译 / 启动时立刻报错，不会静默退到空表。
///
/// 如果 JSON 里没有配置（`{}`），返回空 AliasMap 就行——用户自己的磁盘配置
/// 会在首次启动时播种这份空表，之后手动往里加。
fn default_aliases() -> AliasMap {
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

    #[test]
    fn save_and_roundtrip() {
        let tmp = tempfile_path();
        let mut m = AliasMap::new();
        m.insert(
            "com.example.app".into(),
            vec!["example".into(), "ex".into()],
        );
        save(&tmp, &m).unwrap();
        let loaded = read_json(&tmp).unwrap();
        assert_eq!(
            loaded.get("com.example.app").unwrap(),
            &vec!["example", "ex"]
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let tmp = tempfile_path();
        std::fs::write(&tmp, "this is not json").unwrap();
        let result = read_json(&tmp);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&tmp);
    }

    fn tempfile_path() -> PathBuf {
        let pid = std::process::id();
        PathBuf::from(format!("/tmp/bolt-aliases-test-{pid}.json"))
    }
}
