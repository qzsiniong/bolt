//! 粘贴内容模型与文本分类：Launcher 的 Cmd+V 内容识别。
//!
//! [`PasteContent`] 是宿主公共类型：NSPasteboard 读取（`pin/clipboard.rs`，
//! 实现与测试都在那里）返回它；Launcher 用它做 token 过滤；各模块在
//! `LauncherItem.paste` 上声明自己能处理哪些变体。新增内容类型 = 这里加
//! 变体 + `classify_text`/读取链加一步 + 目标模块声明 handler。

use std::path::{Path, PathBuf};

/// 从剪贴板识别出的内容类型（判定互斥，按优先级只有一个）。
#[derive(Clone, Debug)]
pub(crate) enum PasteContent {
    /// 位图，统一 **PNG** 字节（读取时已转码）。
    Image {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    },
    /// 指向本地文件的路径（Finder 复制文件、或文本本身是存在的文件路径）。
    FilePath(PathBuf),
    /// 能被完整解析成 JSON 值的文本。
    Json(String),
    /// 单行 http(s) URL。
    Url(String),
    /// 其余纯文本。
    Text(String),
}

impl PasteContent {
    /// token 徽章与 UI 提示里显示的类型名。
    pub(crate) fn kind_label(&self) -> &'static str {
        match self {
            PasteContent::Image { .. } => "图片",
            PasteContent::FilePath(_) => "文件",
            PasteContent::Json(_) => "JSON",
            PasteContent::Url(_) => "链接",
            PasteContent::Text(_) => "文本",
        }
    }
}

/// 把一段纯文本分类成更具体的内容类型：URL → JSON → 本地文件路径 → 纯文本。
///
/// 判定尽量保守，宁可归成 Text 也不误判：
/// - URL 要求整段无空白（多行笔记即使以 http 开头也是 Text）；
/// - JSON 要求以 `{`/`[` 开头且能完整解析；
/// - 路径要求 `/` 或 `~` 开头且**真实存在**（是文件）。
pub(crate) fn classify_text(text: String) -> PasteContent {
    let trimmed = text.trim();
    if !trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace) {
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            return PasteContent::Url(text);
        }
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
            return PasteContent::Json(text);
        }
    }
    if trimmed.starts_with('/') || trimmed.starts_with("~/") {
        let expanded = expand_home(trimmed);
        if Path::new(&expanded).is_file() {
            return PasteContent::FilePath(PathBuf::from(expanded));
        }
    }
    PasteContent::Text(text)
}

/// 展开 `~` 前缀为用户主目录（无主目录时原样返回）。
fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str) -> PasteContent {
        classify_text(text.to_string())
    }

    #[test]
    fn classifies_text_variants() {
        assert!(matches!(classify("http://a.b/c"), PasteContent::Url(_)));
        assert!(matches!(classify("https://a.b"), PasteContent::Url(_)));
        // 多行文本即使以 URL 开头也不是链接。
        assert!(matches!(
            classify("https://a.b\n第二行"),
            PasteContent::Text(_)
        ));
        assert!(matches!(classify(r#"{"a": 1}"#), PasteContent::Json(_)));
        assert!(matches!(classify("[1, 2]"), PasteContent::Json(_)));
        // 解析失败 / 半截 JSON 是纯文本。
        assert!(matches!(classify("{oops"), PasteContent::Text(_)));
        assert!(matches!(
            classify("{\"a\": 1} 尾巴"),
            PasteContent::Text(_)
        ));
        // 不存在的路径是纯文本，真实存在的文件是 FilePath。
        assert!(matches!(classify("/nonexistent/path.py"), PasteContent::Text(_)));
        let tmp = crate::test_support::TempDir::new("paste-classify");
        let script = tmp.0.join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert!(matches!(
            classify(script.to_string_lossy().as_ref()),
            PasteContent::FilePath(_)
        ));
        assert!(matches!(classify("hello world"), PasteContent::Text(_)));
    }

    #[test]
    fn kind_labels_are_stable() {
        assert_eq!(PasteContent::Text("t".into()).kind_label(), "文本");
        assert_eq!(
            PasteContent::Image {
                bytes: vec![],
                width: 1,
                height: 1
            }
            .kind_label(),
            "图片"
        );
    }
}
