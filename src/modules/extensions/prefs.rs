//! 扩展的启用 / 禁用偏好：`extensions-disabled.json`。
//!
//! 记录的是扩展 **id**，不是目录名：Launcher 的 item id `ext.<id>.<cmd>` 本来
//! 就不含版本，用户心智里"禁用的就是这个扩展"；同 id 装了多个版本时应当一起
//! 停用，否则升一次版本号就悄悄复活了。
//!
//! 文件坏了、读不到，一律按"全部启用"处理并把原因交给调用方去 Console 说一声
//! —— 偏好丢了比扩展集体消失好得多。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const FILE_NAME: &str = "extensions-disabled.json";

/// `~/Library/Application Support/bolt/extensions-disabled.json`。
pub(crate) fn path() -> PathBuf {
    bolt_model::paths::data_root().join(FILE_NAME)
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct DisabledFile {
    #[serde(default)]
    disabled: Vec<String>,
}

/// 读取禁用列表；第二个返回值是"文件有问题但已降级"的说明（正常时为 `None`）。
pub(crate) fn load(path: &Path) -> (BTreeSet<String>, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (BTreeSet::new(), None);
        }
        Err(error) => {
            return (
                BTreeSet::new(),
                Some(format!(
                    "{FILE_NAME} 读取失败，已按「全部启用」处理：{error}"
                )),
            );
        }
    };
    match parse(&text) {
        Ok(disabled) => (disabled, None),
        Err(error) => (
            BTreeSet::new(),
            Some(format!(
                "{FILE_NAME} 解析失败，已按「全部启用」处理：{error}"
            )),
        ),
    }
}

/// 写盘。调用方负责把错误报给用户（这里不碰 Console，保持纯 IO）。
pub(crate) fn save(path: &Path, disabled: &BTreeSet<String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    std::fs::write(path, render(disabled)).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 解析：空文件视同空表；未知字段忽略（给以后加字段留余地）。
pub(crate) fn parse(json: &str) -> Result<BTreeSet<String>, String> {
    if json.trim().is_empty() {
        return Ok(BTreeSet::new());
    }
    let file: DisabledFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(file.disabled.into_iter().collect())
}

/// 稳定排序（`BTreeSet`）——同一个状态永远生成同一份文本，diff 里不会出现
/// 顺序抖动，也方便人肉手改。
pub(crate) fn render(disabled: &BTreeSet<String>) -> String {
    let file = DisabledFile {
        disabled: disabled.iter().cloned().collect(),
    };
    let mut text = serde_json::to_string_pretty(&file).unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_is_stable() {
        let set: BTreeSet<String> = ["zeta", "alpha"].iter().map(|s| s.to_string()).collect();
        let text = render(&set);
        // 排序稳定：alpha 在前，与插入顺序无关。
        assert!(text.find("alpha").unwrap() < text.find("zeta").unwrap());
        assert_eq!(parse(&text).unwrap(), set);
    }

    #[test]
    fn missing_file_means_nothing_disabled() {
        let tmp = crate::test_support::TempDir::new("prefs-missing");
        let (set, warning) = load(&tmp.0.join(FILE_NAME));
        assert!(set.is_empty());
        assert!(warning.is_none());
    }

    #[test]
    fn broken_json_degrades_to_empty_with_warning() {
        let tmp = crate::test_support::TempDir::new("prefs-broken");
        let path = tmp.0.join(FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        let (set, warning) = load(&path);
        assert!(set.is_empty());
        assert!(warning.is_some_and(|w| w.contains("解析失败")));
    }

    #[test]
    fn empty_file_is_an_empty_list() {
        let tmp = crate::test_support::TempDir::new("prefs-empty");
        let path = tmp.0.join(FILE_NAME);
        std::fs::write(&path, "  \n").unwrap();
        assert!(load(&path).0.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = crate::test_support::TempDir::new("prefs-save");
        let path = tmp.0.join("nested").join(FILE_NAME);
        let set: BTreeSet<String> = ["demo".to_string()].into_iter().collect();
        save(&path, &set).expect("save");
        assert_eq!(load(&path).0, set);
    }
}
