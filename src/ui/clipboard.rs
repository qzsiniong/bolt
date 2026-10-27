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
    /// 位图：剪贴板/文件里的**原始格式**字节（PNG/TIFF/JPEG/GIF，gpui 原生
    /// 支持解码）。读取时不做任何转码——转码慢且无必要；另存、回写剪贴板
    /// 也保持原格式。
    Image {
        bytes: Vec<u8>,
        format: gpui_kit::ImageFormat,
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
    /// token renderer 用的图标 key（渲染端按它映射 Lucide 图标，
    /// 同时作为 `InlineToken` 的 id——其语义就是"引用的资源名"）。
    pub(crate) fn icon_key(&self) -> &'static str {
        match self {
            PasteContent::Image { .. } => "image",
            PasteContent::FilePath(_) => "file",
            PasteContent::Json(_) => "json",
            PasteContent::Url(_) => "link",
            PasteContent::Text(_) => "text",
        }
    }
}

/// 识别阶段的粘贴内容：图片**只识别类型不读字节**（整读 + 转 PNG 慢，
/// 不能卡 Cmd+V 的 UI），其余内容读取便宜（字符串 / stat / 文件头），
/// 识别时就读好。确认（回车）时才把惰性图片读成真内容。
pub(crate) enum PendingPaste {
    /// 图片：字节待读。来源是剪贴板里的图片**文件**时带路径（token 显示
    /// 文件名）；直接复制的位图（截图等）没有文件名。
    Image(Option<PathBuf>),
    /// 其余内容：已读出完整内容。
    Ready(PasteContent),
}

impl PendingPaste {
    /// token 徽章的显示文本：文件显示文件名，JSON/链接/文本显示前几个字符，
    /// 图片没有内容可预览，保持类型名。识别阶段这些都是已知的（图片惰性
    /// 不读字节，也只需要类型名）。
    pub(crate) fn token_text(&self) -> String {
        /// 预览最多保留的字符数（超出截断加省略号）。
        const MAX_CHARS: usize = 12;

        match self {
            PendingPaste::Image(path) => path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "图片".to_string()),
            PendingPaste::Ready(PasteContent::FilePath(path)) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "文件".to_string()),
            PendingPaste::Ready(content) => {
                // Json/Url/Text 的载体都是 String；预览折叠空白（多行 JSON
                // 里的换行会撑坏单行输入框）再截断。
                let text = match content {
                    PasteContent::Json(text)
                    | PasteContent::Url(text)
                    | PasteContent::Text(text) => text,
                    PasteContent::FilePath(_) | PasteContent::Image { .. } => {
                        unreachable!("上面分支已处理")
                    }
                };
                let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if flat.chars().count() <= MAX_CHARS {
                    flat
                } else {
                    flat.chars()
                        .take(MAX_CHARS)
                        .chain(std::iter::once('…'))
                        .collect()
                }
            }
        }
    }

    /// token renderer 用的图标 key（同 [`PasteContent::icon_key`]）。
    pub(crate) fn icon_key(&self) -> &'static str {
        match self {
            PendingPaste::Image(_) => "image",
            PendingPaste::Ready(content) => content.icon_key(),
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
    if !trimmed.is_empty()
        && !trimmed.chars().any(char::is_whitespace)
        && (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
    {
        return PasteContent::Url(text);
    }
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return PasteContent::Json(text);
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
        assert!(matches!(classify("{\"a\": 1} 尾巴"), PasteContent::Text(_)));
        // 不存在的路径是纯文本，真实存在的文件是 FilePath。
        assert!(matches!(
            classify("/nonexistent/path.py"),
            PasteContent::Text(_)
        ));
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
    fn token_text_previews_content() {
        use std::path::PathBuf;
        // 图片：截图位图无文件名 → 类型名；图片文件 → 文件名。
        assert_eq!(PendingPaste::Image(None).token_text(), "图片");
        assert_eq!(
            PendingPaste::Image(Some(PathBuf::from("/tmp/a/shot.png"))).token_text(),
            "shot.png"
        );
        // 文件：文件名（不是全路径）。
        assert_eq!(
            PendingPaste::Ready(PasteContent::FilePath(PathBuf::from("/tmp/a/b.json")))
                .token_text(),
            "b.json"
        );
        // 长文本：截断 + 省略号；多行折叠成单行。
        let t = PendingPaste::Ready(PasteContent::Text(
            "你好，世界！这是一段比较长的文本内容".into(),
        ))
        .token_text();
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 13);
        assert_eq!(
            PendingPaste::Ready(PasteContent::Text("a\n  b".into())).token_text(),
            "a b"
        );
    }
}
